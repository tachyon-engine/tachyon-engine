//! Resumable static Array constructor algorithms.

use core::mem::size_of;

use super::*;

/// GC-owned inputs and cursor state shared by static Array algorithms.
#[derive(Debug)]
pub(crate) struct PendingArrayStatic {
    result: Value,
    constructor: Value,
    retained: Value,
    arguments: Box<[Value]>,
    cursor: u32,
}

impl Trace for PendingArrayStatic {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.result.trace(tracer);
        self.constructor.trace(tracer);
        self.retained.trace(tracer);
        self.arguments.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayStatic {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.arguments.len().saturating_mul(size_of::<Value>())
    }
}

#[derive(Clone, Copy)]
struct ArrayStaticSnapshot {
    result: Value,
    constructor: Value,
    cursor: u32,
    length: u32,
}

impl Isolate {
    /// Captures `Array.of` arguments before any custom constructor can run.
    pub(crate) fn begin_array_of(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let count = usize::try_from(site.argument_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            );
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_array_static_state(PendingArrayStatic {
            result: undefined,
            constructor: site.this_value,
            retained: undefined,
            arguments: arguments.into_boxed_slice(),
            cursor: 0,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_static_state(continuation_site, state)?;
        if self.is_constructor_value(site.this_value)? {
            self.construct_array_static(continuation_site, state)
        } else {
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before Array.of");
            let result = self.create_array_object_with_prototype(prototype)?;
            let state = self.pending_array_static_reference(
                self.read(continuation_site.caller_base, continuation_site.destination)?,
            )?;
            self.set_array_static_value(state, |pending| &mut pending.result, result)?;
            self.advance_array_of(continuation_site, state)
        }
    }

    /// Routes construct, define, and final Set completions to the next algorithm step.
    pub(crate) fn resume_array_static(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        stage: ArrayStaticStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_static_state(site, state)?;
        match stage {
            ArrayStaticStage::Construct => self.finish_array_static_construct(site, state, value),
            ArrayStaticStage::Define => {
                self.increment_array_static_cursor(state)?;
                self.advance_array_of(site, state)
            }
            ArrayStaticStage::FinalLength => self.finish_array_static(site, state),
        }
    }

    /// Invokes a constructor with the exact item count as its sole argument.
    fn construct_array_static(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_static_snapshot(state)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        self.push_array_static_parent(
            site,
            state,
            ArrayStaticStage::Construct,
            snapshot.constructor,
        )?;
        let prefix = match self.create_apply_argument_prefix(
            snapshot.constructor,
            undefined,
            vec![safe_integer_value(u64::from(snapshot.length))],
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_static_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_array_static_parent(
            site,
            state,
            ArrayStaticStage::Construct,
            Value::from_heap_ref(prefix.raw()),
        )?;
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
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Array.of constructor publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_static_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_static_construct(site, state, result)
    }

    /// Validates a custom constructor result before publishing it to the state.
    fn finish_array_static_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        self.set_array_static_value(state, |pending| &mut pending.result, result)?;
        self.advance_array_of(site, state)
    }

    /// Defines remaining items, suspending when an exotic target executes JavaScript.
    fn advance_array_of(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_static_snapshot(state)?;
            if snapshot.cursor >= snapshot.length {
                let length = self.length_atom()?;
                return self.dispatch_array_static_set(
                    site,
                    state,
                    snapshot.result,
                    length.into(),
                    safe_integer_value(u64::from(snapshot.length)),
                );
            }
            let value = self.array_static_argument(state, snapshot.cursor)?;
            self.set_array_static_value(state, |pending| &mut pending.retained, value)?;
            let key = self.safe_integer_property_atom(u64::from(snapshot.cursor))?;
            let descriptor = DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            };
            if self.is_proxy_value(snapshot.result) {
                return self.dispatch_array_static_define(
                    site,
                    state,
                    snapshot.result,
                    key.into(),
                    descriptor.into(),
                );
            }
            self.define_data_property(snapshot.result, key, descriptor)?;
            self.increment_array_static_cursor(state)?;
        }
    }

    /// Returns the constructed object after the observable final length Set succeeds.
    fn finish_array_static(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        let result = self.array_static_snapshot(state)?.result;
        self.write(site.caller_base, site.destination, result)
    }

    /// Performs CreateDataPropertyOrThrow on a Proxy result.
    fn dispatch_array_static_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_static_parent(site, state, ArrayStaticStage::Define, receiver)?;
        let outcome =
            self.dispatch_proxy_define(site, receiver, key, descriptor, ProxyDefineMode::Object);
        if let Err(error) = outcome {
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
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_static_reference(rooted.first())?;
        self.increment_array_static_cursor(state)?;
        self.advance_array_of(site, state)
    }

    /// Performs Set(result, "length", len, true), preserving setters and Proxy traps.
    fn dispatch_array_static_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_static_parent(site, state, ArrayStaticStage::FinalLength, value)?;
        let outcome = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            key,
            value,
            ProxySetMode::ObjectAssign,
        );
        if let Err(error) = outcome {
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
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_static_reference(rooted.first())?;
        self.finish_array_static(site, state)
    }

    /// Pushes one typed parent that roots the operation state and temporary value.
    fn push_array_static_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        stage: ArrayStaticStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_static(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots state in the caller destination before allocation-capable work.
    #[inline]
    fn root_array_static_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates variable-size captured arguments under the complete VM root set.
    fn allocate_array_static_state(
        &mut self,
        pending: PendingArrayStatic,
    ) -> Result<GcRef<PendingArrayStatic>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_array_static,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Validates and recovers one managed static Array state reference.
    pub(crate) fn pending_array_static_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayStatic>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_static)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies scalar state without retaining a managed borrow across safepoints.
    fn array_static_snapshot(
        &mut self,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<ArrayStaticSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_static)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let length = u32::try_from(pending.arguments.len())
                    .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
                Ok(ArrayStaticSnapshot {
                    result: pending.result,
                    constructor: pending.constructor,
                    cursor: pending.cursor,
                    length,
                })
            })
        })
    }

    /// Reads one captured item without exposing its backing slice.
    fn array_static_argument(
        &mut self,
        state: GcRef<PendingArrayStatic>,
        index: u32,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_array_static)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .arguments
                    .get(index as usize)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Advances the item cursor after a successful property definition.
    fn increment_array_static_cursor(
        &mut self,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_static)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.cursor = pending
                    .cursor
                    .checked_add(1)
                    .ok_or(ExecutionError::RegisterAllocationFailed)?;
                Ok(())
            })
        })
    }

    /// Updates one traced state edge and records its generational barrier.
    fn set_array_static_value(
        &mut self,
        state: GcRef<PendingArrayStatic>,
        field: impl FnOnce(&mut PendingArrayStatic) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_static)
                    .map_err(ExecutionError::NoGcBorrow)?;
                *field(pending) = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}
