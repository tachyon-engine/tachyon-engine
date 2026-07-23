//! Resumable Array.prototype.forEach property and callback state machine.

use super::*;

const FOREACH_RECEIVER: usize = 0;
const FOREACH_CALLBACK: usize = 1;
const FOREACH_THIS_ARGUMENT: usize = 2;
const FOREACH_LENGTH: usize = 3;
const FOREACH_NEXT_INDEX: usize = 4;
const FILTER_RESULT: usize = 0;
const FILTER_THIS_ARGUMENT: usize = 1;
const FILTER_NEXT_INDEX: usize = 2;
const FILTER_CONSTRUCTOR: usize = 3;
const FILTER_STATE_COUNT: u8 = 3;

struct ArrayForEachRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for ArrayForEachRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the callback, publishes fixed state, and starts observable length lookup.
    pub(crate) fn begin_array_for_each(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let state = self.allocate_array_for_each_state(NativeCallState {
            values: [
                receiver,
                callback,
                this_argument,
                Value::from_i32(0),
                Value::from_i32(0),
            ],
            count: 5,
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
        let length = self.length_atom()?;
        let value = self.dispatch_array_for_each_get(
            continuation_site,
            state,
            ArrayForEachStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some(value) = value {
            self.resume_array_for_each(
                continuation_site,
                state,
                ArrayForEachStage::Length,
                value,
                receiver,
            )?;
        }
        Ok(())
    }

    /// Creates a filter result and starts the shared observable array-iteration state machine.
    pub(crate) fn begin_array_filter(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let undefined = Value::from_immediate(Immediate::Undefined);
        let filter = self.allocate_array_for_each_state(NativeCallState {
            values: [
                undefined,
                this_argument,
                Value::from_i32(0),
                undefined,
                undefined,
            ],
            count: FILTER_STATE_COUNT,
        })?;
        let state = self.allocate_array_for_each_state(NativeCallState {
            values: [
                receiver,
                callback,
                Value::from_heap_ref(filter.raw()),
                Value::from_i32(0),
                Value::from_i32(0),
            ],
            count: 5,
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
        let length = self.length_atom()?;
        let value = self.dispatch_array_for_each_get(
            continuation_site,
            state,
            ArrayForEachStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some(value) = value {
            self.resume_array_for_each(
                continuation_site,
                state,
                ArrayForEachStage::Length,
                value,
                receiver,
            )?;
        }
        Ok(())
    }

    /// Resumes length, HasProperty, Get, or callback completion from the iterative trampoline.
    pub(crate) fn resume_array_for_each(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ArrayForEachStage,
        value: Value,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            ArrayForEachStage::Length => {
                let guard = NativeContinuation::array_for_each(
                    site,
                    stage,
                    Value::from_heap_ref(state.raw()),
                    value,
                );
                self.fiber
                    .completions
                    .push_native(guard)
                    .map_err(Isolate::completion_stack_error)?;
                let length = self.array_for_each_to_length(value);
                self.pop_native_continuation()?;
                let length = length?;
                self.set_array_for_each_number(state, FOREACH_LENGTH, length)?;
                let callback = self.native_call_state_snapshot(state)?.values[FOREACH_CALLBACK];
                self.resolve_function_object(callback)?;
                if self.array_filter_state(state)?.is_some() {
                    self.begin_array_filter_species(site, state)
                } else {
                    self.advance_array_for_each(site, state)
                }
            }
            ArrayForEachStage::FilterConstructor => {
                if self.is_object_value(value) {
                    let filter = self
                        .array_filter_state(state)?
                        .ok_or(ExecutionError::MissingNativeContinuation)?;
                    self.set_array_for_each_value(filter, FILTER_CONSTRUCTOR, value)?;
                    let species = self
                        .realm
                        .well_known_symbols
                        .species
                        .expect("Symbol.species initializes before Array");
                    let key = self.property_key(species)?;
                    let observed = self.dispatch_array_for_each_get(
                        site,
                        state,
                        ArrayForEachStage::FilterSpecies,
                        value,
                        key,
                    )?;
                    if let Some(observed) = observed {
                        self.resume_array_for_each(
                            site,
                            state,
                            ArrayForEachStage::FilterSpecies,
                            observed,
                            value,
                        )?;
                    }
                    Ok(())
                } else {
                    self.finish_array_filter_species(site, state, value)
                }
            }
            ArrayForEachStage::FilterSpecies => {
                self.finish_array_filter_species(site, state, value)
            }
            ArrayForEachStage::FilterConstruct => {
                self.finish_array_filter_construct(site, state, value)
            }
            ArrayForEachStage::Has => {
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                )?;
                if self.is_truthy_value(value)? {
                    let Some(element) = self.dispatch_array_for_each_element_get(site, state)?
                    else {
                        return Ok(());
                    };
                    let Some(returned) = self.call_array_for_each_callback(site, state, element)?
                    else {
                        return Ok(());
                    };
                    self.select_array_filter_value(state, returned, element)?;
                }
                self.advance_array_for_each(site, state)
            }
            ArrayForEachStage::Get => {
                let Some(returned) = self.call_array_for_each_callback(site, state, value)? else {
                    return Ok(());
                };
                self.select_array_filter_value(state, returned, value)?;
                self.advance_array_for_each(site, state)
            }
            ArrayForEachStage::Callback => {
                self.select_array_filter_value(state, value, retained)?;
                self.advance_array_for_each(site, state)
            }
        }
    }

    /// Starts ArraySpeciesCreate after length conversion and callback validation have completed.
    fn begin_array_filter_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[FOREACH_RECEIVER];
        if !self.is_array_value(receiver)? {
            return self.finish_array_filter_species(
                site,
                state,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        let constructor = self.constructor_atom()?;
        let observed = self.dispatch_array_for_each_get(
            site,
            state,
            ArrayForEachStage::FilterConstructor,
            receiver,
            constructor.into(),
        )?;
        if let Some(observed) = observed {
            self.resume_array_for_each(
                site,
                state,
                ArrayForEachStage::FilterConstructor,
                observed,
                receiver,
            )?;
        }
        Ok(())
    }

    /// Selects the intrinsic Array fallback or constructs the observed species with length zero.
    fn finish_array_filter_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        if matches!(
            constructor.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before filter");
            let result = self.create_array_object_with_prototype(prototype)?;
            let filter = self
                .array_filter_state(state)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            self.set_array_for_each_value(filter, FILTER_RESULT, result)?;
            return self.advance_array_for_each(site, state);
        }
        let filter = self
            .array_filter_state(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_array_for_each_value(filter, FILTER_CONSTRUCTOR, constructor)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let prefix =
            self.create_apply_argument_prefix(constructor, undefined, vec![Value::from_i32(0)])?;
        self.push_array_for_each_parent(
            site,
            state,
            ArrayForEachStage::FilterConstruct,
            constructor,
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
                .expect("Array species constructor publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_filter_construct(site, state, result)
    }

    /// Publishes a custom species result only after Construct has returned an object.
    fn finish_array_filter_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        let filter = self
            .array_filter_state(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_array_for_each_value(filter, FILTER_RESULT, result)?;
        self.advance_array_for_each(site, state)
    }

    /// Runs synchronous elements in a loop and exits whenever observable work suspends the fiber.
    fn advance_array_for_each(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            let pending = self.native_call_state_snapshot(state)?;
            let length = exact_nonnegative_integer(pending.values[FOREACH_LENGTH])?;
            let index = exact_nonnegative_integer(pending.values[FOREACH_NEXT_INDEX])?;
            if index >= length {
                let result = if let Some(filter) = self.array_filter_state(state)? {
                    self.native_call_state_snapshot(filter)?.values[FILTER_RESULT]
                } else {
                    Value::from_immediate(Immediate::Undefined)
                };
                return self.write(site.caller_base, site.destination, result);
            }
            self.set_array_for_each_number(state, FOREACH_NEXT_INDEX, index + 1)?;
            let key = Value::from_f64(index as f64);
            let Some(has) = self.dispatch_array_for_each_has(
                site,
                state,
                pending.values[FOREACH_RECEIVER],
                key,
            )?
            else {
                return Ok(());
            };
            if !self.is_truthy_value(has)? {
                continue;
            }
            let Some(element) = self.dispatch_array_for_each_element_get(site, state)? else {
                return Ok(());
            };
            let Some(returned) = self.call_array_for_each_callback(site, state, element)? else {
                return Ok(());
            };
            self.select_array_filter_value(state, returned, element)?;
        }
    }

    /// Dispatches element Get using the index already advanced before HasProperty observation.
    fn dispatch_array_for_each_element_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let next = exact_nonnegative_integer(pending.values[FOREACH_NEXT_INDEX])?;
        let key = self.property_key_atom(Value::from_f64((next - 1) as f64))?;
        self.dispatch_array_for_each_get(
            site,
            state,
            ArrayForEachStage::Get,
            pending.values[FOREACH_RECEIVER],
            key.into(),
        )
    }

    /// Calls the callback with `(value, index, receiver)` while state and value stay rooted.
    fn call_array_for_each_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let next = exact_nonnegative_integer(pending.values[FOREACH_NEXT_INDEX])?;
        let index = Value::from_f64((next - 1) as f64);
        let this_argument = if let Some(filter) = self.array_filter_state(state)? {
            self.native_call_state_snapshot(filter)?.values[FILTER_THIS_ARGUMENT]
        } else {
            pending.values[FOREACH_THIS_ARGUMENT]
        };
        let continuation = NativeContinuation::array_for_each(
            site,
            ArrayForEachStage::Callback,
            Value::from_heap_ref(state.raw()),
            value,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Isolate::completion_stack_error)?;
        let prefix = match self.create_apply_argument_prefix(
            pending.values[FOREACH_CALLBACK],
            this_argument,
            vec![value, index, pending.values[FOREACH_RECEIVER]],
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: pending.values[FOREACH_CALLBACK],
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 3,
            argument_count: 3,
            this_value: this_argument,
            new_target: Value::from_immediate(Immediate::Undefined),
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
                .expect("Array forEach callback publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        Ok(Some(returned))
    }

    /// Publishes an Array parent continuation around a Proxy-aware property Get.
    fn dispatch_array_for_each_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ArrayForEachStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_for_each_parent(site, state, stage, receiver)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return Ok(None);
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        debug_assert_eq!(
            continuation.kind(),
            NativeContinuationKind::ArrayForEach(stage)
        );
        Ok(Some(value))
    }

    /// Publishes an Array parent continuation around an ordinary or Proxy HasProperty operation.
    fn dispatch_array_for_each_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        key: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_for_each_parent(site, state, ArrayForEachStage::Has, key)?;
        let outcome = self.dispatch_has_property(site, receiver, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return Ok(None);
        }
        self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some(value))
    }

    fn push_array_for_each_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ArrayForEachStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_for_each(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Allocates the fixed receiver/callback/length/index state under the complete VM root set.
    fn allocate_array_for_each_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = ArrayForEachRoots {
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

    /// Stores only exact nonnegative numeric immediates in the fixed iteration state.
    fn set_array_for_each_number(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        number: u64,
    ) -> Result<(), ExecutionError> {
        let value = Value::from_f64(number as f64);
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = value;
                Ok(())
            })
        })
    }

    /// Updates one traced Array iteration state slot and records its managed edge.
    fn set_array_for_each_value(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Applies the existing ToLength boundary after observable length Get completes.
    fn array_for_each_to_length(&mut self, value: Value) -> Result<u64, ExecutionError> {
        let number = self.convert_to_number(value)?;
        let number =
            numeric_value(number).ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
            return Ok(MAX_SAFE_INTEGER);
        }
        Ok(number.floor() as u64)
    }

    /// Returns the filter side-state when an iteration state represents Array.prototype.filter.
    fn array_filter_state(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<Option<GcRef<NativeCallState>>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let Some(raw) = pending.values[FOREACH_CALLBACK + 1].as_heap_ref() else {
            return Ok(None);
        };
        let Ok(filter) = self
            .heap
            .checked_reference(raw, self.types.native_call_state)
        else {
            return Ok(None);
        };
        let snapshot = self.native_call_state_snapshot(filter)?;
        Ok((snapshot.count == FILTER_STATE_COUNT).then_some(filter))
    }

    /// Appends a selected value to the filter result with one indexed write and length update.
    fn append_array_filter_value(
        &mut self,
        filter: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let index = exact_nonnegative_integer(
            self.native_call_state_snapshot(filter)?.values[FILTER_NEXT_INDEX],
        )?;
        let key = self.safe_integer_property_atom(index)?;
        let result = self.native_call_state_snapshot(filter)?.values[FILTER_RESULT];
        self.set_own_data_property(result, key, value)?;
        if self.is_array_value(result)? {
            let length = self.length_atom()?;
            self.set_own_data_property(result, length, safe_integer_value(index + 1))?;
        }
        self.set_array_for_each_number(filter, FILTER_NEXT_INDEX, index + 1)
    }

    /// Appends the retained element when a filter callback completed truthily.
    fn select_array_filter_value(
        &mut self,
        state: GcRef<NativeCallState>,
        returned: Value,
        element: Value,
    ) -> Result<(), ExecutionError> {
        if let Some(filter) = self.array_filter_state(state)?
            && self.is_truthy_value(returned)?
        {
            self.append_array_filter_value(filter, element)?;
        }
        Ok(())
    }
}

fn exact_nonnegative_integer(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return Err(ExecutionError::UnsupportedNumberConversion(value));
    }
    Ok(number as u64)
}
