//! Intrinsic Promise combinator state and indexed reaction handlers.

mod storage;

use super::super::*;

impl Isolate {
    /// Selects the guarded intrinsic Array fast path or the resumable iterable path.
    pub(crate) fn begin_promise_all(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_promise_combinator(site, PromiseCombinatorKind::All)
    }

    /// Starts `Promise.race` on the same observable iterator/capability protocol driver.
    pub(crate) fn begin_promise_race(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_promise_combinator(site, PromiseCombinatorKind::Race)
    }

    /// Selects constructor handling and the only currently proven empty-Array fast path.
    fn begin_promise_combinator(
        &mut self,
        site: &CallSite,
        kind: PromiseCombinatorKind,
    ) -> Result<(), ExecutionError> {
        let intrinsic = self
            .realm
            .promise_constructor
            .expect("Promise initializes before combinators");
        if !self.is_constructor_value(site.this_value)? {
            return Err(ExecutionError::NonConstructor(site.this_value));
        }
        let iterable = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if site.this_value != intrinsic {
            return self.begin_generic_promise_combinator(site, site.this_value, iterable, kind);
        }
        if kind == PromiseCombinatorKind::All
            && self.can_use_promise_all_array_fast_path(intrinsic, iterable)?
        {
            return self.begin_intrinsic_promise_all_array(site, intrinsic, iterable);
        }
        self.begin_intrinsic_promise_combinator(site, intrinsic, iterable, kind)
    }

    /// Implements the guarded intrinsic Array-iterator fast path for `Promise.all`.
    fn begin_intrinsic_promise_all_array(
        &mut self,
        site: &CallSite,
        intrinsic: Value,
        iterable: Value,
    ) -> Result<(), ExecutionError> {
        let length = self.promise_all_array_length(iterable)?;
        let state = self.create_promise_combinator_state(
            site,
            intrinsic,
            iterable,
            length,
            true,
            PromiseCombinatorKind::All,
        )?;
        let pending = self.promise_combinator_snapshot(state)?;
        let aggregate = pending.promise;
        let values = pending.values;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        if length == 0 {
            return self.begin_promise_resolution(
                aggregate,
                values,
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                PromiseResolutionMode::StaticResolve,
            );
        }
        for index in 0..length {
            let key = self.property_key_atom(safe_integer_value(index))?;
            let value = self
                .get_data_property(iterable, key)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            let input = self.promise_all_input_promise(value)?;
            self.set_promise_combinator_temporary(state, input)?;
            let child = self.create_promise(
                PromiseState::Pending,
                Value::from_immediate(Immediate::Undefined),
            )?;
            let (state, fulfilled, rejected) =
                self.allocate_promise_all_handlers(state, input, child, index)?;
            let (state, attachment) = self.allocate_promise_all_attachment(
                state,
                NativeCallState {
                    values: [
                        input,
                        child,
                        fulfilled,
                        rejected,
                        Value::from_immediate(Immediate::Undefined),
                    ],
                    count: 4,
                },
            )?;
            self.set_promise_combinator_temporary(state, Value::from_heap_ref(attachment.raw()))?;
            self.perform_promise_then_with_capability(
                input,
                Some(fulfilled),
                Some(rejected),
                child,
            )?;
        }
        self.write(site.caller_base, site.destination, aggregate)
    }

    /// Starts the observable GetPromiseResolve and iterator protocol for intrinsic `%Promise%`.
    fn begin_intrinsic_promise_combinator(
        &mut self,
        site: &CallSite,
        intrinsic: Value,
        iterable: Value,
        kind: PromiseCombinatorKind,
    ) -> Result<(), ExecutionError> {
        let state =
            self.create_promise_combinator_state(site, intrinsic, iterable, 1, false, kind)?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let resolve = PropertyKey::Atom(self.intern_intrinsic_name(b"resolve")?);
        let result = self.dispatch_promise_combinator_get(
            native_site,
            state,
            PromiseCombinatorStage::ResolveGet,
            intrinsic,
            resolve,
        );
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let continuation = NativeContinuation::promise_combinator(
                    native_site,
                    PromiseCombinatorStage::ResolveGet,
                    Value::from_heap_ref(state.raw()),
                    intrinsic,
                );
                if self.reject_promise_combinator_execution_error(continuation, &error)? {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Runs NewPromiseCapability for a custom constructor before entering the shared iterator path.
    fn begin_generic_promise_combinator(
        &mut self,
        site: &CallSite,
        constructor: Value,
        iterable: Value,
        kind: PromiseCombinatorKind,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let values = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array initializes before Promise.all"),
        )?;
        self.write(site.caller_base, site.destination, values)?;
        let (capability, executor) = self.allocate_generic_promise_capability()?;
        let state = self.allocate_pending_promise_combinator(PendingPromiseCombinator {
            promise: undefined,
            values,
            temporary: executor,
            capability: Value::from_heap_ref(capability.raw()),
            capability_resolve: undefined,
            capability_reject: undefined,
            constructor,
            promise_resolve: undefined,
            iterable,
            iterator: undefined,
            next: undefined,
            iterator_result: undefined,
            current: undefined,
            index: 0,
            remaining: 1,
            kind,
            stage: PromiseCombinatorStage::CapabilityConstructor,
            iterator_done: false,
            return_promise_after_capability_call: true,
            settled: false,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let (state, prefix) = self.allocate_promise_combinator_argument_prefix(
            state,
            constructor,
            undefined,
            vec![executor],
        )?;
        let constructor = self.bound_function_snapshot(prefix)?.call_target;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_combinator(
                native_site,
                PromiseCombinatorStage::CapabilityConstructor,
                Value::from_heap_ref(state.raw()),
                constructor,
            ))
            .map_err(Isolate::completion_stack_error)?;
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
                    .expect("custom Promise.all constructor publishes one frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        self.pop_native_continuation()?;
        let promise = self.read(site.caller_base, site.destination)?;
        self.resume_promise_combinator(
            native_site,
            state,
            PromiseCombinatorStage::CapabilityConstructor,
            promise,
        )
    }

    /// Checks every mutable identity assumed by the no-protocol-call Array fast path.
    fn can_use_promise_all_array_fast_path(
        &mut self,
        intrinsic: Value,
        iterable: Value,
    ) -> Result<bool, ExecutionError> {
        let direct_array = iterable
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.array).is_ok());
        if !direct_array {
            return Ok(false);
        }
        let resolve_key = PropertyKey::Atom(self.intern_intrinsic_name(b"resolve")?);
        let resolve = self.resolve_property_read(intrinsic, resolve_key)?;
        let expected_resolve = self
            .realm
            .promise_resolve
            .expect("Promise.resolve initializes before Promise.all");
        if !matches!(resolve, PropertyRead::Data(value) if value == expected_resolve) {
            return Ok(false);
        }
        let iterator = self
            .realm
            .well_known_symbols
            .iterator
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let iterator_key = self.property_key(iterator)?;
        let iterator = self.resolve_property_read(iterable, iterator_key)?;
        let expected_iterator = self
            .realm
            .array_values
            .expect("Array values initializes before Promise.all");
        if !matches!(iterator, PropertyRead::Data(value) if value == expected_iterator) {
            return Ok(false);
        }
        // A non-empty fast path also needs a watchpoint proving every input's observable `then`
        // remains builtin. Until that substrate exists, only the empty case is semantically safe.
        self.promise_all_array_length(iterable)
            .map(|length| length == 0)
    }

    /// Allocates the aggregate Promise, result Array, and typed state in root-safe order.
    fn create_promise_combinator_state(
        &mut self,
        site: &CallSite,
        constructor: Value,
        iterable: Value,
        remaining: u64,
        iterator_done: bool,
        kind: PromiseCombinatorKind,
    ) -> Result<GcRef<PendingPromiseCombinator>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let aggregate = self.create_promise(PromiseState::Pending, undefined)?;
        self.write(site.caller_base, site.destination, aggregate)?;
        let (capability_resolve, capability_reject) = if iterator_done {
            (undefined, undefined)
        } else {
            let capability = self.create_promise_capability_arguments(aggregate)?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(capability.raw()),
            )?;
            let capability = self.native_call_state_snapshot(capability)?;
            (capability.values[0], capability.values[1])
        };
        let values = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array initializes before Promise.all"),
        )?;
        self.allocate_pending_promise_combinator(PendingPromiseCombinator {
            promise: aggregate,
            values,
            temporary: undefined,
            capability: undefined,
            capability_resolve,
            capability_reject,
            constructor,
            promise_resolve: undefined,
            iterable,
            iterator: undefined,
            next: undefined,
            iterator_result: undefined,
            current: undefined,
            index: 0,
            remaining,
            kind,
            stage: PromiseCombinatorStage::ResolveGet,
            iterator_done,
            return_promise_after_capability_call: true,
            settled: false,
        })
    }

    /// Applies one indexed fulfillment or rejection to the shared Promise.all state.
    pub(crate) fn call_promise_all_handler(
        &mut self,
        site: &CallSite,
        element: GcRef<PromiseCombinatorElement>,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        let Some((state, index)) = self.take_promise_combinator_element(element)? else {
            return self.write_undefined(site);
        };
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let pending = self.promise_combinator_snapshot(state)?;
        if pending.settled {
            return self.write_undefined(site);
        }
        if rejected {
            self.set_promise_combinator_settled(state)?;
            self.settle_promise(pending.promise, PromiseState::Rejected, argument)?;
            return self.write_undefined(site);
        }
        let key = self.property_key_atom(safe_integer_value(index))?;
        self.set_own_data_property(pending.values, key, argument)?;
        let remaining = self.decrement_promise_combinator_remaining(state)?;
        if remaining == 0 {
            return self.finish_promise_combinator_fulfill(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                state,
                pending.values,
                false,
            );
        }
        self.write_undefined(site)
    }

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
                if pending.kind == PromiseCombinatorKind::All {
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
                let (state, fulfilled, unused_rejected) = self.allocate_promise_all_handlers(
                    state,
                    pending.current,
                    pending.promise,
                    pending.index,
                )?;
                let rejected = pending.capability_reject;
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
                self.resolve_function_object(value)?;
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
        let iterator_step_abrupt = matches!(
            continuation.kind(),
            NativeContinuationKind::PromiseCombinator(
                PromiseCombinatorStage::DoneGet | PromiseCombinatorStage::ValueGet
            )
        );
        if iterator_step_abrupt {
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

    /// Settles the aggregate rejection and restores the public result into the caller register.
    fn reject_promise_combinator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.promise_combinator_snapshot(state)?;
        if !pending.settled {
            self.set_promise_combinator_settled(state)?;
            if pending.capability.as_immediate() != Some(Immediate::Undefined) {
                self.update_promise_combinator(state, |pending| {
                    pending.return_promise_after_capability_call = true
                })?;
                return self.call_promise_combinator(
                    site,
                    state,
                    PromiseCombinatorStage::CapabilityRejectCall,
                    pending.capability_reject,
                    Value::from_immediate(Immediate::Undefined),
                    &[reason],
                );
            }
            self.settle_promise(pending.promise, PromiseState::Rejected, reason)?;
        }
        self.write(site.caller_base, site.destination, pending.promise)
    }

    /// Calls a generic capability resolve or settles the intrinsic aggregate directly.
    fn finish_promise_combinator_fulfill(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        values: Value,
        return_promise: bool,
    ) -> Result<(), ExecutionError> {
        let pending = self.promise_combinator_snapshot(state)?;
        if pending.settled {
            return if return_promise {
                self.write(site.caller_base, site.destination, pending.promise)
            } else {
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                )
            };
        }
        self.set_promise_combinator_settled(state)?;
        if pending.capability.as_immediate() != Some(Immediate::Undefined) {
            self.update_promise_combinator(state, |pending| {
                pending.return_promise_after_capability_call = return_promise
            })?;
            return self.call_promise_combinator(
                site,
                state,
                PromiseCombinatorStage::CapabilityResolveCall,
                pending.capability_resolve,
                Value::from_immediate(Immediate::Undefined),
                &[values],
            );
        }
        self.begin_promise_resolution(
            pending.promise,
            values,
            site,
            if return_promise {
                PromiseResolutionMode::StaticResolve
            } else {
                PromiseResolutionMode::ResolverCall
            },
        )
    }

    /// Reads one protocol property under a typed Promise combinator parent continuation.
    fn dispatch_promise_combinator_get(
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
    fn call_promise_combinator(
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
    fn call_promise_combinator_next(
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
    fn write_undefined(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(Immediate::Undefined),
        )
    }
}
