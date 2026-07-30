//! Resumable CreateDynamicFunction argument conversion and prototype selection.

use core::mem::size_of;

use super::*;

/// GC-owned roots and exact-capacity conversion storage for one constructor call.
#[derive(Debug)]
pub(crate) struct PendingDynamicFunction {
    callee: Value,
    new_target: Value,
    function: Value,
    arguments: Box<[Value]>,
    strings: Box<[Value]>,
    cursor: u32,
    kind: DynamicFunctionKind,
}

impl Trace for PendingDynamicFunction {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.callee.trace(tracer);
        self.new_target.trace(tracer);
        self.function.trace(tracer);
        self.arguments.trace(tracer);
        self.strings.trace(tracer);
    }
}

impl GcExternalMemory for PendingDynamicFunction {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.arguments
            .len()
            .saturating_add(self.strings.len())
            .saturating_mul(size_of::<Value>())
    }
}

#[derive(Clone, Copy)]
struct DynamicFunctionSnapshot {
    callee: Value,
    new_target: Value,
    function: Value,
    cursor: u32,
    argument_count: u32,
    kind: DynamicFunctionKind,
}

impl Isolate {
    /// Captures all constructor arguments before any observable ToString operation.
    pub(crate) fn begin_dynamic_function(
        &mut self,
        site: &CallSite,
        kind: DynamicFunctionKind,
    ) -> Result<(), ExecutionError> {
        let count = usize::try_from(site.argument_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(count)
            .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            );
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let new_target = if site.new_target == undefined {
            site.callee
        } else {
            site.new_target
        };
        let state = self.allocate_pending_dynamic_function(PendingDynamicFunction {
            callee: site.callee,
            new_target,
            function: undefined,
            arguments: arguments.into_boxed_slice(),
            strings: vec![undefined; count].into_boxed_slice(),
            cursor: 0,
            kind,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_dynamic_function(native_site, state)?;
        self.advance_dynamic_function(native_site, state, None)
    }

    /// Resumes after an object argument has produced its primitive ToString input.
    pub(crate) fn resume_dynamic_function_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDynamicFunction>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_dynamic_function(site, state)?;
        self.advance_dynamic_function(site, state, Some(primitive))
    }

    /// Freezes arguments in order, then calls the embedding compiler exactly once.
    fn advance_dynamic_function(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingDynamicFunction>,
        mut returned: Option<Value>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.dynamic_function_snapshot(state)?;
            if snapshot.cursor == snapshot.argument_count {
                return self.compile_dynamic_function(site, state, snapshot);
            }
            let value = match returned.take() {
                Some(value) => value,
                None => self.dynamic_function_argument(state, snapshot.cursor)?,
            };
            if self.is_object_value(value) {
                return self.dispatch_object_primitive_conversion(
                    ConversionConsumer::DynamicFunctionArgument,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    value,
                    site.call_site,
                );
            }
            let string = self.error_message_string(value)?;
            state = self.pending_dynamic_function_reference(
                self.read(site.caller_base, site.destination)?,
            )?;
            self.set_dynamic_function_string(state, snapshot.cursor, string)?;
            self.increment_dynamic_function_cursor(state)?;
        }
    }

    /// Materializes frozen UTF-16 input outside the heap, then begins prototype lookup.
    fn compile_dynamic_function(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDynamicFunction>,
        snapshot: DynamicFunctionSnapshot,
    ) -> Result<(), ExecutionError> {
        let source = self.dynamic_function_source(state)?;
        let callback = self
            .dynamic_function_callback
            .ok_or(ExecutionError::UnsupportedDynamicFunctionConstructor)?;
        let realm = self.realm_for_callable(snapshot.callee)?;
        let function = callback(self, realm, snapshot.kind, source)?;
        let state = self
            .pending_dynamic_function_reference(self.read(site.caller_base, site.destination)?)?;
        self.set_dynamic_function_result(state, function)?;
        self.dispatch_dynamic_function_prototype(site, state)
    }

    /// Reads newTarget.prototype with the existing Proxy/accessor-aware dispatcher.
    fn dispatch_dynamic_function_prototype(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDynamicFunction>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.dynamic_function_snapshot(state)?;
        let continuation = NativeContinuation::dynamic_function_prototype(
            site,
            Value::from_heap_ref(state.raw()),
            snapshot.new_target,
        );
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Isolate::completion_stack_error)?;
        let prototype = self.prototype_atom()?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(
            site,
            snapshot.new_target,
            snapshot.new_target,
            prototype.into(),
        ) {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let candidate = self.read(site.caller_base, site.destination)?;
        self.resume_dynamic_function_prototype(continuation, candidate)
    }

    /// Applies GetPrototypeFromConstructor fallback and publishes the compiled closure.
    pub(crate) fn resume_dynamic_function_prototype(
        &mut self,
        continuation: NativeContinuation,
        candidate: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_dynamic_function_reference(continuation.first())?;
        let snapshot = self.dynamic_function_snapshot(state)?;
        let prototype = if self.is_object_value(candidate) {
            candidate
        } else {
            let realm = self.realm_for_callable(snapshot.new_target)?;
            let kind = match snapshot.kind {
                DynamicFunctionKind::Ordinary => IntrinsicPrototypeKind::Function,
                DynamicFunctionKind::Generator => IntrinsicPrototypeKind::GeneratorFunction,
                DynamicFunctionKind::Async => IntrinsicPrototypeKind::AsyncFunction,
                DynamicFunctionKind::AsyncGenerator => {
                    IntrinsicPrototypeKind::AsyncGeneratorFunction
                }
            };
            self.realm_intrinsic_prototype(realm, kind)
                .ok_or(ExecutionError::MissingNativeContinuation)?
        };
        self.set_function_internal_prototype(snapshot.function, prototype)?;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            snapshot.function,
        )
    }

    /// Builds the callback payload while the managed state remains rooted in the destination.
    fn dynamic_function_source(
        &mut self,
        state: GcRef<PendingDynamicFunction>,
    ) -> Result<DynamicFunctionSource, ExecutionError> {
        let snapshot = self.dynamic_function_snapshot(state)?;
        if snapshot.argument_count == 0 {
            return Ok(DynamicFunctionSource {
                parameters: Box::new([]),
                body: Box::new([]),
            });
        }
        let parameter_count = snapshot.argument_count - 1;
        let mut parameters = Vec::new();
        parameters
            .try_reserve_exact(parameter_count as usize)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..parameter_count {
            let string = self.dynamic_function_string(state, index)?;
            parameters.push(self.string_value_to_utf16(string)?.into_boxed_slice());
        }
        let body = self.dynamic_function_string(state, parameter_count)?;
        Ok(DynamicFunctionSource {
            parameters: parameters.into_boxed_slice(),
            body: self.string_value_to_utf16(body)?.into_boxed_slice(),
        })
    }

    fn allocate_pending_dynamic_function(
        &mut self,
        pending: PendingDynamicFunction,
    ) -> Result<GcRef<PendingDynamicFunction>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_dynamic_function,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_dynamic_function_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingDynamicFunction>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_dynamic_function)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn root_dynamic_function(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDynamicFunction>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    fn dynamic_function_snapshot(
        &mut self,
        state: GcRef<PendingDynamicFunction>,
    ) -> Result<DynamicFunctionSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_dynamic_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(DynamicFunctionSnapshot {
                    callee: pending.callee,
                    new_target: pending.new_target,
                    function: pending.function,
                    cursor: pending.cursor,
                    argument_count: pending.arguments.len() as u32,
                    kind: pending.kind,
                })
            })
        })
    }

    fn dynamic_function_argument(
        &mut self,
        state: GcRef<PendingDynamicFunction>,
        index: u32,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_dynamic_function)
                    .map(|pending| pending.arguments[index as usize])
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn dynamic_function_string(
        &mut self,
        state: GcRef<PendingDynamicFunction>,
        index: u32,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_dynamic_function)
                    .map(|pending| pending.strings[index as usize])
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_dynamic_function_string(
        &mut self,
        state: GcRef<PendingDynamicFunction>,
        index: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_dynamic_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.strings[index as usize] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    fn increment_dynamic_function_cursor(
        &mut self,
        state: GcRef<PendingDynamicFunction>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_dynamic_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.cursor += 1;
                Ok(())
            })
        })
    }

    fn set_dynamic_function_result(
        &mut self,
        state: GcRef<PendingDynamicFunction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_dynamic_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.function = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}
