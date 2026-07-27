//! Generic Promise capability capture and reaction settlement.

use super::*;

const STATIC_CAPABILITY: usize = 0;
const STATIC_RESOLUTION: usize = 1;

struct PromiseStaticResolveRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for PromiseStaticResolveRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Implements Promise.withResolvers using the existing managed capability/resolver records.
    pub(crate) fn promise_with_resolvers(
        &mut self,
        constructor: Value,
    ) -> Result<Value, ExecutionError> {
        if !self.is_constructor_value(constructor)? {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(constructor, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or(
                self.realm
                    .promise_prototype
                    .expect("Promise prototype initializes before withResolvers"),
            );
        let promise = self.create_promise_with_prototype(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
            prototype,
        )?;
        let state = self.create_promise_capability_arguments(promise)?;
        let pending = self.native_call_state_snapshot(state)?;
        let result = self.create_ordinary_object()?;
        let attributes = |value| DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        let promise_atom = self.intern_intrinsic_name(b"promise")?;
        let resolve_atom = self.intern_intrinsic_name(b"resolve")?;
        let reject_atom = self.intern_intrinsic_name(b"reject")?;
        self.define_data_property(result, promise_atom, attributes(promise))?;
        self.define_data_property(result, resolve_atom, attributes(pending.values[0]))?;
        self.define_data_property(result, reject_atom, attributes(pending.values[1]))?;
        Ok(result)
    }

    /// Runs generic NewPromiseCapability for Promise.resolve without recursing into bytecode.
    pub(crate) fn begin_generic_promise_resolve(
        &mut self,
        site: &CallSite,
        constructor: Value,
        resolution: Value,
    ) -> Result<(), ExecutionError> {
        self.begin_generic_promise_static(
            site,
            constructor,
            resolution,
            PromiseStaticResolveStage::ResolveConstructor,
        )
    }

    /// Runs generic NewPromiseCapability for Promise.reject without recursing into bytecode.
    pub(crate) fn begin_generic_promise_reject(
        &mut self,
        site: &CallSite,
        constructor: Value,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        self.begin_generic_promise_static(
            site,
            constructor,
            reason,
            PromiseStaticResolveStage::RejectConstructor,
        )
    }

    /// Publishes shared capability state before any constructor-prefix allocation can collect.
    fn begin_generic_promise_static(
        &mut self,
        site: &CallSite,
        constructor: Value,
        argument: Value,
        stage: PromiseStaticResolveStage,
    ) -> Result<(), ExecutionError> {
        debug_assert!(matches!(
            stage,
            PromiseStaticResolveStage::ResolveConstructor
                | PromiseStaticResolveStage::RejectConstructor
        ));
        if !self.is_constructor_value(constructor)? {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        let (capability, executor) = self.allocate_generic_promise_capability()?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_promise_static_resolve_state(NativeCallState {
            values: [
                Value::from_heap_ref(capability.raw()),
                argument,
                executor,
                undefined,
                undefined,
            ],
            count: 3,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_static_resolve(
                continuation_site,
                stage,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Isolate::completion_stack_error)?;
        // Publish the state before creating the bound prefix: that allocation may trigger GC.
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, vec![executor])
        {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.construct_site(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: constructor,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 1,
            argument_count: 1,
            this_value: undefined,
            new_target: constructor,
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            if self.fiber.frames.len() != frame_depth {
                let frame = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("generic Promise.resolve constructor publishes a frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let promise = self.read(site.caller_base, site.destination)?;
        self.resume_generic_promise_resolve(continuation, stage, promise)
    }

    /// Resumes generic Promise.resolve after either the constructor or resolve callback returns.
    pub(crate) fn resume_generic_promise_resolve(
        &mut self,
        continuation: NativeContinuation,
        stage: PromiseStaticResolveStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            PromiseStaticResolveStage::ConstructorPrototype => {
                Err(ExecutionError::MissingNativeContinuation)
            }
            PromiseStaticResolveStage::ResolveConstructor => {
                self.finish_generic_static_constructor(continuation, value, false)
            }
            PromiseStaticResolveStage::RejectConstructor => {
                self.finish_generic_static_constructor(continuation, value, true)
            }
            PromiseStaticResolveStage::ResolveCallback
            | PromiseStaticResolveStage::RejectCallback => {
                self.finish_generic_promise_resolve(continuation)
            }
            PromiseStaticResolveStage::TryConstructor
            | PromiseStaticResolveStage::TryCallback
            | PromiseStaticResolveStage::TryResolve
            | PromiseStaticResolveStage::TryReject => {
                self.resume_promise_try(continuation, stage, value)
            }
        }
    }

    /// Validates constructor capture and invokes the selected settlement callback.
    fn finish_generic_static_constructor(
        &mut self,
        continuation: NativeContinuation,
        promise: Value,
        reject: bool,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(promise) {
            return Err(ExecutionError::NotObject(promise));
        }
        let state = self.native_call_state_reference(continuation.first())?;
        let pending = self.native_call_state_snapshot(state)?;
        let capability = self.promise_capability_reference(pending.values[STATIC_CAPABILITY])?;
        let snapshot = self.promise_capability_snapshot(capability)?;
        self.resolve_function_object(snapshot.resolve)?;
        self.resolve_function_object(snapshot.reject)?;
        let callback = if reject {
            snapshot.reject
        } else {
            snapshot.resolve
        };
        self.set_promise_capability_promise(capability, promise)?;
        let arguments = self.allocate_promise_job_arguments(pending.values[STATIC_RESOLUTION])?;
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_static_resolve(
                continuation.site(),
                if reject {
                    PromiseStaticResolveStage::RejectCallback
                } else {
                    PromiseStaticResolveStage::ResolveCallback
                },
                continuation.first(),
            ))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: continuation.site().caller_base,
            destination: continuation.site().destination,
            callee: callback,
            argument_base: 0,
            argument_source: Some(arguments),
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: continuation.site().call_site,
        }) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            if self.fiber.frames.len() != frame_depth {
                let frame = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("generic Promise.resolve callback publishes a frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        self.finish_generic_promise_resolve(continuation)
    }

    /// Returns the constructor result after its resolve callback completes normally.
    fn finish_generic_promise_resolve(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        let pending = self.native_call_state_snapshot(state)?;
        let capability = self.promise_capability_reference(pending.values[STATIC_CAPABILITY])?;
        let promise = self.promise_capability_snapshot(capability)?.promise;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            promise,
        )?;
        if let Some(parent) = self.fiber.completions.last_native()
            && parent.kind() == NativeContinuationKind::PromiseFinallyResolve
            && parent.site().destination.checked_add(1) == Some(continuation.site().destination)
        {
            let parent = self.pop_native_continuation()?;
            let state = self.native_call_state_reference(parent.first())?;
            return self.finish_promise_finally_resolved(parent, state, promise);
        }
        Ok(())
    }

    /// Allocates one fixed generic resolve state under the complete VM root set.
    pub(crate) fn allocate_promise_static_resolve_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = PromiseStaticResolveRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one empty capability and its strict two-argument capture executor.
    pub(crate) fn allocate_generic_promise_capability(
        &mut self,
    ) -> Result<(GcRef<PromiseCapability>, Value), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = GenericPromiseCapabilityRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            capability: None,
            executor: undefined,
        };
        let capability = self
            .heap
            .try_allocate_with_gc(
                self.types.promise_capability,
                0,
                0,
                PromiseCapability {
                    promise: undefined,
                    resolve: undefined,
                    reject: undefined,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.capability = Some(capability);
        let prototype = roots
            .vm
            .realm
            .function_prototype
            .expect("Function prototype initializes before Promise capabilities");
        roots.executor = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::PromiseCapabilityExecutor(capability),
                    prototype_or_home_object: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)?;
        Ok((capability, roots.executor))
    }

    /// Captures custom resolve/reject functions without retaining a heap borrow across barriers.
    pub(crate) fn call_promise_capability_executor(
        &mut self,
        site: &CallSite,
        capability: GcRef<PromiseCapability>,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let resolve = self.call_argument(site, 0)?.unwrap_or(undefined);
        let reject = self.call_argument(site, 1)?.unwrap_or(undefined);
        let current = self.promise_capability_snapshot(capability)?;
        if current.resolve != undefined || current.reject != undefined {
            return Err(ExecutionError::InvalidPropertyRedefinition(
                Value::from_heap_ref(capability.raw()),
            ));
        }
        self.set_promise_capability_functions(capability, resolve, reject)?;
        self.write(site.caller_base, site.destination, undefined)
    }

    /// Resolves a checked capability value to its dedicated managed record type.
    pub(crate) fn promise_capability_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PromiseCapability>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.promise_capability)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies capability fields so arbitrary callback validation cannot retain a heap borrow.
    pub(crate) fn promise_capability_snapshot(
        &mut self,
        capability: GcRef<PromiseCapability>,
    ) -> Result<PromiseCapability, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let capability = scope.root(capability).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(capability, self.types.promise_capability)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Publishes the object returned by the custom constructor after callable validation.
    pub(crate) fn set_promise_capability_promise(
        &mut self,
        capability: GcRef<PromiseCapability>,
        promise: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let capability = scope.root(capability).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(capability, self.types.promise_capability)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .promise = promise;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(capability, promise)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Allocates one traced argument source for a Promise job callback.
    pub(crate) fn allocate_promise_job_arguments(
        &mut self,
        argument: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                NativeCallState {
                    values: [argument, undefined, undefined, undefined, undefined],
                    count: 1,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Settles a reaction capability after its handler returns and resumes checkpoint dispatch.
    pub(crate) fn finish_promise_reaction(
        &mut self,
        continuation: NativeContinuation,
        returned: Value,
    ) -> Result<(), ExecutionError> {
        self.begin_promise_reaction_settlement(
            continuation.first(),
            returned,
            false,
            continuation.site(),
        )
        .map(|_| ())
    }

    /// Routes a thrown reaction handler through the capability's rejection operation.
    pub(crate) fn begin_promise_reaction_rejection(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        self.begin_promise_reaction_settlement(
            continuation.first(),
            reason,
            true,
            continuation.site(),
        )
        .map(|_| ())
    }

    /// Selects the intrinsic resolution fast path or invokes one generic capability callback.
    pub(crate) fn begin_promise_reaction_settlement(
        &mut self,
        capability: Value,
        argument: Value,
        reject: bool,
        site: NativeContinuationSite,
    ) -> Result<bool, ExecutionError> {
        let Some(generic) = self.generic_promise_capability_snapshot(capability)? else {
            if reject {
                self.settle_promise(capability, PromiseState::Rejected, argument)?;
                self.finish_promise_reaction_job(site)?;
                return Ok(false);
            }
            let frame_depth = self.fiber.frames.len();
            self.begin_promise_resolution(
                capability,
                argument,
                site,
                PromiseResolutionMode::Reaction,
            )?;
            return Ok(self.fiber.frames.len() != frame_depth);
        };
        let callback = if reject {
            generic.reject
        } else {
            generic.resolve
        };
        self.call_generic_promise_capability(capability, callback, argument, site)
    }

    /// Calls one captured capability function and preserves the active job across bytecode frames.
    fn call_generic_promise_capability(
        &mut self,
        capability: Value,
        callback: Value,
        argument: Value,
        site: NativeContinuationSite,
    ) -> Result<bool, ExecutionError> {
        let arguments = self.allocate_promise_job_arguments(argument)?;
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_capability_call(
                site, capability,
            ))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: callback,
            argument_base: 0,
            argument_source: Some(arguments),
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            self.promise_jobs.finish_active();
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("generic Promise capability callback publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(true);
        }
        if self.fiber.completions.len() <= completion_depth {
            return Ok(false);
        }
        let continuation = self.pop_native_continuation()?;
        self.finish_promise_capability_call(continuation)?;
        Ok(false)
    }

    /// Completes a normally returned generic callback at the original checkpoint site.
    pub(crate) fn finish_promise_capability_call(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        self.finish_promise_reaction_job(continuation.site())
    }

    /// Publishes the two executor arguments and applies both generational barriers.
    fn set_promise_capability_functions(
        &mut self,
        capability: GcRef<PromiseCapability>,
        resolve: Value,
        reject: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let capability = scope.root(capability).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let capability = no_gc
                    .borrow_mut(capability, self.types.promise_capability)
                    .map_err(ExecutionError::NoGcBorrow)?;
                capability.resolve = resolve;
                capability.reject = reject;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(capability, resolve)
                .map_err(ExecutionError::HeapReference)?;
            scope
                .write_value_barrier(capability, reject)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Detects the generic record without penalizing direct Promise capability settlement.
    pub(crate) fn generic_promise_capability_snapshot(
        &mut self,
        capability: Value,
    ) -> Result<Option<PromiseCapability>, ExecutionError> {
        let Some(raw) = capability.as_heap_ref() else {
            return Ok(None);
        };
        let Ok(capability) = self
            .heap
            .checked_reference(raw, self.types.promise_capability)
        else {
            return Ok(None);
        };
        self.promise_capability_snapshot(capability).map(Some)
    }

    /// Releases the active reaction root and lets the entry return resume the same checkpoint.
    fn finish_promise_reaction_job(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<(), ExecutionError> {
        self.promise_jobs.finish_active();
        self.fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?
            .pc = site.call_site;
        Ok(())
    }
}
