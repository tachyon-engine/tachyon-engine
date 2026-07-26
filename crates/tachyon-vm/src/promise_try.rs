//! Promise.try capability capture, callback dispatch, and settlement.

use super::*;

const TRY_CAPABILITY: usize = 0;
const TRY_ARGUMENTS: usize = 1;

impl Isolate {
    /// Runs Promise.try through an intrinsic fast path or generic NewPromiseCapability state.
    pub(crate) fn begin_promise_try(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let constructor = site.this_value;
        if !self.is_constructor_value(constructor)? {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let callback = self.call_argument(site, 0)?.unwrap_or(undefined);
        let argument_count = site.argument_count.saturating_sub(1);
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(argument_count as usize)
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        for index in 0..argument_count {
            arguments.push(
                self.call_argument(site, index + 1)?
                    .expect("Promise.try callback argument remains in range"),
            );
        }
        let intrinsic = self
            .realm
            .promise_constructor
            .expect("Promise initializes before Promise.try");
        if constructor == intrinsic {
            let promise = self.create_promise(PromiseState::Pending, undefined)?;
            let state = self.allocate_promise_static_resolve_state(NativeCallState {
                values: [promise, callback, undefined, constructor, undefined],
                count: 4,
            })?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            let prefix = self.create_apply_argument_prefix(callback, undefined, arguments)?;
            self.set_promise_try_state_value(
                state,
                TRY_ARGUMENTS,
                Value::from_heap_ref(prefix.raw()),
            )?;
            return self.call_promise_try_callback(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                state,
            );
        }
        self.begin_generic_promise_try(site, constructor, callback, arguments)
    }

    /// Captures a custom constructor capability before invoking the Promise.try callback.
    fn begin_generic_promise_try(
        &mut self,
        site: &CallSite,
        constructor: Value,
        callback: Value,
        arguments: Vec<Value>,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let (capability, executor) = self.allocate_generic_promise_capability()?;
        let state = self.allocate_promise_static_resolve_state(NativeCallState {
            values: [
                Value::from_heap_ref(capability.raw()),
                callback,
                executor,
                constructor,
                undefined,
            ],
            count: 4,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let callback_prefix = self.create_apply_argument_prefix(callback, undefined, arguments)?;
        self.set_promise_try_state_value(
            state,
            TRY_ARGUMENTS,
            Value::from_heap_ref(callback_prefix.raw()),
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
                PromiseStaticResolveStage::TryConstructor,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Isolate::completion_stack_error)?;
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
                    .expect("Promise.try constructor publishes one frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let promise = self.read(site.caller_base, site.destination)?;
        self.finish_promise_try_constructor(continuation, promise)
    }

    /// Resumes Promise.try after its custom constructor, callback, or capability call returns.
    pub(crate) fn resume_promise_try(
        &mut self,
        continuation: NativeContinuation,
        stage: PromiseStaticResolveStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            PromiseStaticResolveStage::TryConstructor => {
                self.finish_promise_try_constructor(continuation, value)
            }
            PromiseStaticResolveStage::TryCallback => {
                self.finish_promise_try_callback(continuation, value)
            }
            PromiseStaticResolveStage::TryResolve | PromiseStaticResolveStage::TryReject => {
                self.finish_promise_try_settlement(continuation)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Validates generic capability capture and advances into the callback call.
    fn finish_promise_try_constructor(
        &mut self,
        continuation: NativeContinuation,
        promise: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(promise) {
            return Err(ExecutionError::NotObject(promise));
        }
        let state = self.native_call_state_reference(continuation.first())?;
        let pending = self.native_call_state_snapshot(state)?;
        let capability = self.promise_capability_reference(pending.values[TRY_CAPABILITY])?;
        let snapshot = self.promise_capability_snapshot(capability)?;
        self.resolve_function_object(snapshot.resolve)?;
        self.resolve_function_object(snapshot.reject)?;
        self.set_promise_capability_promise(capability, promise)?;
        self.call_promise_try_callback(continuation.site(), state)
    }

    /// Calls the callback with its exact variadic suffix stored in one immutable managed prefix.
    fn call_promise_try_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let prefix = self.promise_try_argument_prefix_reference(pending.values[TRY_ARGUMENTS])?;
        let callback = self.bound_function_snapshot(prefix)?;
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_static_resolve(
                site,
                PromiseStaticResolveStage::TryCallback,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: callback.call_target,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: callback.argument_count,
            argument_count: callback.argument_count,
            this_value: callback.bound_this,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            let continuation = self.pop_native_continuation()?;
            let Some(kind) = execution_error_kind(&error) else {
                return Err(error);
            };
            let thrown = self.create_native_error(kind, None)?;
            return self.reject_promise_try_callback(continuation, thrown);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            if self.fiber.frames.len() != frame_depth {
                let frame = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("Promise.try callback publishes one frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.finish_promise_try_callback(continuation, returned)
    }

    /// Resolves a normal callback completion through the intrinsic or generic capability path.
    fn finish_promise_try_callback(
        &mut self,
        continuation: NativeContinuation,
        returned: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        let pending = self.native_call_state_snapshot(state)?;
        if let Some(capability) =
            self.generic_promise_capability_snapshot(pending.values[TRY_CAPABILITY])?
        {
            return self.call_promise_try_capability(
                continuation.site(),
                state,
                capability.resolve,
                returned,
                PromiseStaticResolveStage::TryResolve,
            );
        }
        self.begin_promise_resolution(
            pending.values[TRY_CAPABILITY],
            returned,
            continuation.site(),
            PromiseResolutionMode::StaticResolve,
        )
    }

    /// Converts a callback throw into capability rejection while preserving callback ordering.
    pub(crate) fn reject_promise_try_callback(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        let pending = self.native_call_state_snapshot(state)?;
        if let Some(capability) =
            self.generic_promise_capability_snapshot(pending.values[TRY_CAPABILITY])?
        {
            return self.call_promise_try_capability(
                continuation.site(),
                state,
                capability.reject,
                reason,
                PromiseStaticResolveStage::TryReject,
            );
        }
        let promise = pending.values[TRY_CAPABILITY];
        self.settle_promise(promise, PromiseState::Rejected, reason)?;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            promise,
        )
    }

    /// Calls a generic capability function and returns its promise after normal completion.
    fn call_promise_try_capability(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        callback: Value,
        argument: Value,
        stage: PromiseStaticResolveStage,
    ) -> Result<(), ExecutionError> {
        debug_assert!(matches!(
            stage,
            PromiseStaticResolveStage::TryResolve | PromiseStaticResolveStage::TryReject
        ));
        let arguments = self.allocate_promise_job_arguments(argument)?;
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_static_resolve(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
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
                    .expect("Promise.try capability callback publishes one frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        self.finish_promise_try_settlement(continuation)
    }

    /// Returns the generic capability promise after resolve or reject completes normally.
    fn finish_promise_try_settlement(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        let pending = self.native_call_state_snapshot(state)?;
        let capability = self.promise_capability_reference(pending.values[TRY_CAPABILITY])?;
        let promise = self.promise_capability_snapshot(capability)?.promise;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            promise,
        )
    }

    /// Resolves a packed Promise.try callback argument prefix to its managed backing type.
    fn promise_try_argument_prefix_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<BoundFunctionData>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.bound_function)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Updates one Promise.try state edge and records the owner barrier.
    fn set_promise_try_state_value(
        &mut self,
        state: GcRef<NativeCallState>,
        index: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[index] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}
