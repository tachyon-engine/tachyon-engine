use super::super::super::*;

impl Isolate {
    /// Selects the guarded intrinsic Array fast path or the resumable iterable path.
    pub(crate) fn begin_promise_all(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_promise_combinator(site, PromiseCombinatorKind::All)
    }

    /// Starts `Promise.allSettled` without duplicating iterator or capability protocol state.
    pub(crate) fn begin_promise_all_settled(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_promise_combinator(site, PromiseCombinatorKind::AllSettled)
    }

    /// Starts `Promise.any` on the shared observable iterator/capability protocol driver.
    pub(crate) fn begin_promise_any(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_promise_combinator(site, PromiseCombinatorKind::Any)
    }

    /// Starts `Promise.race` on the same observable iterator/capability protocol driver.
    pub(crate) fn begin_promise_race(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_promise_combinator(site, PromiseCombinatorKind::Race)
    }

    /// Selects constructor handling and the only currently proven empty-Array fast path.
    pub(super) fn begin_promise_combinator(
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
    pub(super) fn begin_intrinsic_promise_all_array(
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
    pub(super) fn begin_intrinsic_promise_combinator(
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
    pub(super) fn begin_generic_promise_combinator(
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
    pub(super) fn can_use_promise_all_array_fast_path(
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
            .agent
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
    pub(super) fn create_promise_combinator_state(
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
}
