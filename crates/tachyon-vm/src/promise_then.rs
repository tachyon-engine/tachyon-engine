//! Observable Promise.prototype.then SpeciesConstructor staging.

use super::*;

const THEN_SOURCE: usize = 0;
const THEN_ON_FULFILLED: usize = 1;
const THEN_ON_REJECTED: usize = 2;
const THEN_CONSTRUCTOR: usize = 3;
const THEN_CAPABILITY: usize = 4;

struct PromiseThenRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for PromiseThenRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the Promise brand and begins the observable constructor lookup exactly once.
    pub(crate) fn begin_promise_then(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.promise_snapshot(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let on_fulfilled = self
            .call_argument(site, 0)?
            .filter(|value| self.resolve_function_object(*value).is_ok())
            .unwrap_or(undefined);
        let on_rejected = self
            .call_argument(site, 1)?
            .filter(|value| self.resolve_function_object(*value).is_ok())
            .unwrap_or(undefined);
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [
                site.this_value,
                on_fulfilled,
                on_rejected,
                undefined,
                undefined,
            ],
            count: 4,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let constructor = self.constructor_atom()?;
        self.dispatch_promise_then_get(
            continuation_site,
            state,
            PromiseThenStage::Constructor,
            site.this_value,
            constructor.into(),
        )
    }

    /// Continues SpeciesConstructor after either observable property lookup completes.
    pub(crate) fn resume_promise_then(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: PromiseThenStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        match stage {
            PromiseThenStage::Constructor => {
                if value.as_immediate() == Some(Immediate::Undefined) {
                    return self.finish_intrinsic_promise_then(site, state);
                }
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.set_promise_then_constructor(state, value)?;
                let species = self
                    .realm
                    .well_known_symbols
                    .species
                    .expect("Symbol.species initializes before Promise");
                let key = self.property_key(species)?;
                self.dispatch_promise_then_get(site, state, PromiseThenStage::Species, value, key)
            }
            PromiseThenStage::Species => {
                if matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                ) || value
                    == self
                        .realm
                        .promise_constructor
                        .expect("Promise constructor initializes before then")
                {
                    return self.finish_intrinsic_promise_then(site, state);
                }
                if self.resolve_function_object(value).is_err() {
                    return Err(ExecutionError::NonConstructor(value));
                }
                self.begin_custom_promise_capability(site, state, value)
            }
            PromiseThenStage::Capability => {
                self.finish_custom_promise_capability(site, state, value)
            }
        }
    }

    /// Enters a custom species constructor so abrupt construction propagates before capability work.
    fn begin_custom_promise_capability(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        let (capability, executor) = self.allocate_generic_promise_capability()?;
        self.set_promise_then_value(
            state,
            THEN_CAPABILITY,
            Value::from_heap_ref(capability.raw()),
        )?;
        let prefix = self.create_apply_argument_prefix(
            constructor,
            Value::from_immediate(Immediate::Undefined),
            vec![executor],
        )?;
        let continuation = NativeContinuation::promise_then(
            site,
            PromiseThenStage::Capability,
            Value::from_heap_ref(state.raw()),
            constructor,
        );
        self.fiber
            .completions
            .push_native(continuation)
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
            this_value: Value::from_immediate(Immediate::Undefined),
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
                .expect("custom Promise species constructor publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        self.pop_native_continuation()?;
        let promise = self.read(site.caller_base, site.destination)?;
        self.finish_custom_promise_capability(site, state, promise)
    }

    /// Validates and publishes the custom constructor result before attaching reactions.
    fn finish_custom_promise_capability(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        promise: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(promise) {
            return Err(ExecutionError::NotObject(promise));
        }
        let pending = self.native_call_state_snapshot(state)?;
        let capability = self.promise_capability_reference(pending.values[THEN_CAPABILITY])?;
        let snapshot = self.promise_capability_snapshot(capability)?;
        self.resolve_function_object(snapshot.resolve)?;
        self.resolve_function_object(snapshot.reject)?;
        self.set_promise_capability_promise(capability, promise)?;
        self.write(site.caller_base, site.destination, promise)?;
        self.perform_promise_then_with_capability(
            pending.values[THEN_SOURCE],
            callable_option(pending.values[THEN_ON_FULFILLED]),
            callable_option(pending.values[THEN_ON_REJECTED]),
            Value::from_heap_ref(capability.raw()),
        )
    }

    /// Creates the intrinsic result Promise only after SpeciesConstructor selects `%Promise%`.
    fn finish_intrinsic_promise_then(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let on_fulfilled = callable_option(pending.values[THEN_ON_FULFILLED]);
        let on_rejected = callable_option(pending.values[THEN_ON_REJECTED]);
        self.perform_intrinsic_promise_then(
            pending.values[THEN_SOURCE],
            on_fulfilled,
            on_rejected,
            site,
        )?;
        Ok(())
    }

    /// Wraps ordinary/accessor/Proxy Get with a Promise.then parent continuation.
    fn dispatch_promise_then_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: PromiseThenStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_then(
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
        self.resume_promise_then(site, state, stage, value)
    }

    /// Allocates the fixed source/handlers/constructor state under all isolate roots.
    fn allocate_promise_then_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = PromiseThenRoots {
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

    /// Publishes the constructor edge with a barrier before the @@species lookup allocates.
    fn set_promise_then_constructor(
        &mut self,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_promise_then_value(state, THEN_CONSTRUCTOR, constructor)
    }

    /// Updates one fixed Promise.then state slot and records its managed edge.
    fn set_promise_then_value(
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

#[inline]
fn callable_option(value: Value) -> Option<Value> {
    (value.as_immediate() != Some(Immediate::Undefined)).then_some(value)
}
