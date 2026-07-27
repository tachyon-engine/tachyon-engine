use super::super::super::*;

impl Isolate {
    /// Advances the generic Promise.all protocol after one observable operation completes.
    pub(crate) fn resume_promise_combinator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        stage: PromiseCombinatorStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.update_promise_combinator(state, |pending| pending.stage = stage)?;
        match stage {
            PromiseCombinatorStage::CapabilityConstructor => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                let capability_value = self.promise_combinator_snapshot(state)?.capability;
                let capability = self.promise_capability_reference(capability_value)?;
                let snapshot = self.promise_capability_snapshot(capability)?;
                self.resolve_function_object(snapshot.resolve)?;
                self.resolve_function_object(snapshot.reject)?;
                self.set_promise_capability_promise(capability, value)?;
                self.set_promise_combinator_value(state, value, |pending, value| {
                    pending.promise = value
                })?;
                self.set_promise_combinator_value(state, snapshot.resolve, |pending, value| {
                    pending.capability_resolve = value
                })?;
                self.set_promise_combinator_value(state, snapshot.reject, |pending, value| {
                    pending.capability_reject = value
                })?;
                let pending = self.promise_combinator_snapshot(state)?;
                let resolve = PropertyKey::Atom(self.intern_intrinsic_name(b"resolve")?);
                self.dispatch_promise_combinator_get(
                    site,
                    state,
                    PromiseCombinatorStage::ResolveGet,
                    pending.constructor,
                    resolve,
                )
            }
            PromiseCombinatorStage::ResolveGet => {
                self.resolve_function_object(value)?;
                self.set_promise_combinator_value(state, value, |pending, value| {
                    pending.promise_resolve = value
                })?;
                let pending = self.promise_combinator_snapshot(state)?;
                let iterator = self
                    .realm
                    .well_known_symbols
                    .iterator
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                let key = self.property_key(iterator)?;
                self.dispatch_promise_combinator_get(
                    site,
                    state,
                    PromiseCombinatorStage::IteratorMethodGet,
                    pending.iterable,
                    key,
                )
            }
            PromiseCombinatorStage::IteratorMethodGet => {
                self.resolve_function_object(value)?;
                let iterable = self.promise_combinator_snapshot(state)?.iterable;
                self.set_promise_combinator_temporary(state, value)?;
                self.call_promise_combinator(
                    site,
                    state,
                    PromiseCombinatorStage::IteratorMethodCall,
                    value,
                    iterable,
                    &[],
                )
            }
            PromiseCombinatorStage::IteratorMethodCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.set_promise_combinator_value(state, value, |pending, value| {
                    pending.iterator = value
                })?;
                let next = PropertyKey::Atom(self.intern_intrinsic_name(b"next")?);
                self.dispatch_promise_combinator_get(
                    site,
                    state,
                    PromiseCombinatorStage::NextGet,
                    value,
                    next,
                )
            }
            PromiseCombinatorStage::NextGet => {
                self.resolve_function_object(value)?;
                self.set_promise_combinator_value(state, value, |pending, value| {
                    pending.next = value
                })?;
                self.call_promise_combinator_next(site, state)
            }
            PromiseCombinatorStage::NextCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.set_promise_combinator_value(state, value, |pending, value| {
                    pending.iterator_result = value
                })?;
                let done = PropertyKey::Atom(self.intern_intrinsic_name(b"done")?);
                self.dispatch_promise_combinator_get(
                    site,
                    state,
                    PromiseCombinatorStage::DoneGet,
                    value,
                    done,
                )
            }
            PromiseCombinatorStage::DoneGet => {
                if self.is_truthy_value(value)? {
                    self.update_promise_combinator(state, |pending| pending.iterator_done = true)?;
                    let pending = self.promise_combinator_snapshot(state)?;
                    if pending.kind == PromiseCombinatorKind::Race {
                        return self.write(site.caller_base, site.destination, pending.promise);
                    }
                    let remaining = self.decrement_promise_combinator_remaining(state)?;
                    let pending = self.promise_combinator_snapshot(state)?;
                    if remaining == 0 && !pending.settled {
                        if pending.kind == PromiseCombinatorKind::Any {
                            let (state, error) =
                                self.create_promise_any_aggregate_error(site, state)?;
                            return self.finish_promise_combinator_reject(site, state, error, true);
                        }
                        return self.finish_promise_combinator_fulfill(
                            site,
                            state,
                            pending.values,
                            true,
                        );
                    }
                    return self.write(site.caller_base, site.destination, pending.promise);
                }
                let result = self.promise_combinator_snapshot(state)?.iterator_result;
                let key = PropertyKey::Atom(self.intern_intrinsic_name(b"value")?);
                self.dispatch_promise_combinator_get(
                    site,
                    state,
                    PromiseCombinatorStage::ValueGet,
                    result,
                    key,
                )
            }
            PromiseCombinatorStage::ValueGet => {
                let pending = self.promise_combinator_snapshot(state)?;
                if matches!(
                    pending.kind,
                    PromiseCombinatorKind::All
                        | PromiseCombinatorKind::AllSettled
                        | PromiseCombinatorKind::Any
                ) {
                    let key = self.property_key_atom(safe_integer_value(pending.index))?;
                    self.set_own_data_property(
                        pending.values,
                        key,
                        Value::from_immediate(Immediate::Undefined),
                    )?;
                    self.increment_promise_combinator_remaining(state)?;
                }
                self.set_promise_combinator_value(state, value, |pending, value| {
                    pending.current = value
                })?;
                self.call_promise_combinator(
                    site,
                    state,
                    PromiseCombinatorStage::ResolveCall,
                    pending.promise_resolve,
                    pending.constructor,
                    &[value],
                )
            }
            PromiseCombinatorStage::ResolveCall => {
                self.set_promise_combinator_value(state, value, |pending, value| {
                    pending.current = value
                })?;
                let then = PropertyKey::Atom(self.intern_intrinsic_name(b"then")?);
                self.dispatch_promise_combinator_get(
                    site,
                    state,
                    PromiseCombinatorStage::ThenGet,
                    value,
                    then,
                )
            }
            PromiseCombinatorStage::ThenGet => {
                self.resolve_function_object(value)?;
                let pending = self.promise_combinator_snapshot(state)?;
                if pending.kind == PromiseCombinatorKind::Race {
                    let (state, attachment) = self.allocate_promise_all_attachment(
                        state,
                        NativeCallState {
                            values: [
                                pending.current,
                                pending.capability_resolve,
                                pending.capability_reject,
                                value,
                                Value::from_immediate(Immediate::Undefined),
                            ],
                            count: 4,
                        },
                    )?;
                    let values = self.native_call_state_snapshot(attachment)?.values;
                    self.set_promise_combinator_temporary(
                        state,
                        Value::from_heap_ref(attachment.raw()),
                    )?;
                    return self.call_promise_combinator(
                        site,
                        state,
                        PromiseCombinatorStage::ThenCall,
                        values[3],
                        values[0],
                        &[values[1], values[2]],
                    );
                }
                let (state, generated_fulfilled, unused_rejected) = self
                    .allocate_promise_all_handlers(
                        state,
                        pending.current,
                        pending.promise,
                        pending.index,
                    )?;
                let fulfilled = if pending.kind == PromiseCombinatorKind::Any {
                    pending.capability_resolve
                } else {
                    generated_fulfilled
                };
                let rejected = if matches!(
                    pending.kind,
                    PromiseCombinatorKind::AllSettled | PromiseCombinatorKind::Any
                ) {
                    unused_rejected
                } else {
                    pending.capability_reject
                };
                let (state, attachment) = self.allocate_promise_all_attachment(
                    state,
                    NativeCallState {
                        values: [pending.current, fulfilled, rejected, value, unused_rejected],
                        count: 5,
                    },
                )?;
                let attachment_values = self.native_call_state_snapshot(attachment)?.values;
                let fulfilled = attachment_values[1];
                let rejected = attachment_values[2];
                let then = attachment_values[3];
                let current = attachment_values[0];
                self.set_promise_combinator_temporary(
                    state,
                    Value::from_heap_ref(attachment.raw()),
                )?;
                self.call_promise_combinator(
                    site,
                    state,
                    PromiseCombinatorStage::ThenCall,
                    then,
                    current,
                    &[fulfilled, rejected],
                )
            }
            PromiseCombinatorStage::ThenCall => {
                self.update_promise_combinator(state, |pending| {
                    pending.index = pending.index.saturating_add(1)
                })?;
                self.call_promise_combinator_next(site, state)
            }
            PromiseCombinatorStage::CloseReturnGet => {
                let pending = self.promise_combinator_snapshot(state)?;
                if matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                ) {
                    return self.reject_promise_combinator(site, state, pending.current);
                }
                // IteratorClose suppresses a non-callable `return` error when the
                // original completion was already abrupt, and forwards that
                // original reason to the aggregate capability.  Checking here
                // keeps the synchronous property-read path on that rejection
                // route instead of leaking a native TypeError.
                if !self.is_callable_value(value)? {
                    return self.reject_promise_combinator(site, state, pending.current);
                }
                self.call_promise_combinator(
                    site,
                    state,
                    PromiseCombinatorStage::CloseReturnCall,
                    value,
                    pending.iterator,
                    &[],
                )
            }
            PromiseCombinatorStage::CloseReturnCall => {
                let reason = self.promise_combinator_snapshot(state)?.current;
                self.reject_promise_combinator(site, state, reason)
            }
            PromiseCombinatorStage::CapabilityResolveCall
            | PromiseCombinatorStage::CapabilityRejectCall => {
                let pending = self.promise_combinator_snapshot(state)?;
                if pending.return_promise_after_capability_call {
                    self.write(site.caller_base, site.destination, pending.promise)
                } else {
                    self.write(site.caller_base, site.destination, value)
                }
            }
        }
    }

    /// Converts an abrupt protocol callback into rejection, closing an active iterator first.
    pub(crate) fn reject_or_close_promise_combinator(
        &mut self,
        continuation: NativeContinuation,
        thrown: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.pending_promise_combinator_reference(continuation.first())?;
        let pending = self.promise_combinator_snapshot(state)?;
        let closing = matches!(
            continuation.kind(),
            NativeContinuationKind::PromiseCombinator(
                PromiseCombinatorStage::CloseReturnGet | PromiseCombinatorStage::CloseReturnCall
            )
        );
        let reason = if closing { pending.current } else { thrown };
        if matches!(
            continuation.kind(),
            NativeContinuationKind::PromiseCombinator(
                PromiseCombinatorStage::CapabilityResolveCall
            )
        ) {
            self.update_promise_combinator(state, |pending| pending.settled = false)?;
            return self.reject_promise_combinator(site, state, reason);
        }
        let no_close_abrupt = matches!(
            continuation.kind(),
            NativeContinuationKind::PromiseCombinator(
                PromiseCombinatorStage::NextGet
                    | PromiseCombinatorStage::NextCall
                    | PromiseCombinatorStage::DoneGet
                    | PromiseCombinatorStage::ValueGet
            )
        );
        if no_close_abrupt {
            self.update_promise_combinator(state, |pending| pending.iterator_done = true)?;
            return self.reject_promise_combinator(site, state, reason);
        }
        if !closing && !pending.iterator_done && self.is_object_value(pending.iterator) {
            self.set_promise_combinator_value(state, reason, |pending, reason| {
                pending.current = reason;
                pending.iterator_done = true;
            })?;
            let return_key = PropertyKey::Atom(self.intern_intrinsic_name(b"return")?);
            return self.dispatch_promise_combinator_get(
                site,
                state,
                PromiseCombinatorStage::CloseReturnGet,
                pending.iterator,
                return_key,
            );
        }
        self.reject_promise_combinator(site, state, reason)
    }

    /// Maps a synchronous VM semantic error to the same rejection path as a thrown callback value.
    pub(crate) fn reject_promise_combinator_execution_error(
        &mut self,
        continuation: NativeContinuation,
        error: &ExecutionError,
    ) -> Result<bool, ExecutionError> {
        let reason = match error {
            ExecutionError::HostThrown(value) => *value,
            _ => {
                let Some(kind) = execution_error_kind(error) else {
                    return Ok(false);
                };
                self.create_native_error(kind, None)?
            }
        };
        self.reject_or_close_promise_combinator(continuation, reason)?;
        Ok(true)
    }

    /// Reads one protocol property under a typed Promise combinator parent continuation.
    pub(super) fn dispatch_promise_combinator_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        stage: PromiseCombinatorStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_combinator(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                receiver,
            ))
            .map_err(Isolate::completion_stack_error)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key);
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
        self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_promise_combinator(site, state, stage, value)
    }

    /// Calls one cached protocol function without representing JavaScript calls on Rust's stack.
    pub(super) fn call_promise_combinator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        stage: PromiseCombinatorStage,
        callee: Value,
        receiver: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        self.update_promise_combinator(state, |pending| pending.stage = stage)?;
        let (state, prefix, callee, receiver) = if arguments.is_empty() {
            (state, None, callee, receiver)
        } else {
            let (state, prefix) = self.allocate_promise_combinator_argument_prefix(
                state,
                callee,
                receiver,
                arguments.to_vec(),
            )?;
            let snapshot = self.bound_function_snapshot(prefix)?;
            (
                state,
                Some(prefix),
                snapshot.call_target,
                snapshot.bound_this,
            )
        };
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_combinator(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                callee,
            ))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: prefix,
            argument_prefix_offset: 0,
            argument_prefix_count: u32::try_from(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
            argument_count: u32::try_from(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
            this_value: receiver,
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
                    .expect("Promise combinator call publishes its callee frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.resume_promise_combinator(site, state, stage, returned)
    }

    /// Calls the cached iterator next method for the current generic iteration.
    pub(super) fn call_promise_combinator_next(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
    ) -> Result<(), ExecutionError> {
        let pending = self.promise_combinator_snapshot(state)?;
        self.call_promise_combinator(
            site,
            state,
            PromiseCombinatorStage::NextCall,
            pending.next,
            pending.iterator,
            &[],
        )
    }

    #[inline(always)]
    pub(super) fn write_undefined(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(Immediate::Undefined),
        )
    }
}
