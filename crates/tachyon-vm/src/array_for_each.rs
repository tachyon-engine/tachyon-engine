//! Resumable Array iteration property, callback, species, and predicate state machine.

use super::*;

mod find;
mod map;
mod output;
mod reduce;
mod search;

const FOREACH_RECEIVER: usize = 0;
const FOREACH_CALLBACK: usize = 1;
const FOREACH_THIS_ARGUMENT: usize = 2;
const FOREACH_LENGTH: usize = 3;
const FOREACH_NEXT_INDEX: usize = 4;
const OUTPUT_RESULT: usize = 0;
const OUTPUT_THIS_ARGUMENT: usize = 1;
const OUTPUT_NEXT_INDEX: usize = 2;
const OUTPUT_CONSTRUCTOR: usize = 3;
const OUTPUT_PENDING_VALUE: usize = 4;
const FILTER_STATE_COUNT: u8 = 3;
const MAP_STATE_COUNT: u8 = 4;
const PREDICATE_THIS_ARGUMENT: usize = 0;
const PREDICATE_CONTINUE_TRUTHINESS: usize = 1;
const PREDICATE_STATE_COUNT: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayOutputKind {
    Filter,
    Map,
}

impl ArrayOutputKind {
    #[inline(always)]
    fn construction_length(self, source_length: u64) -> u64 {
        match self {
            Self::Filter => 0,
            Self::Map => source_length,
        }
    }
}

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
    /// Starts Array.prototype.every/some with a compact side-state for the short-circuit contract.
    pub(crate) fn begin_array_predicate(
        &mut self,
        site: &CallSite,
        continue_truthiness: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let predicate = self.allocate_array_for_each_state(NativeCallState {
            values: [
                this_argument,
                Value::from_immediate(if continue_truthiness {
                    Immediate::True
                } else {
                    Immediate::False
                }),
                Value::from_i32(0),
                Value::from_i32(0),
                Value::from_i32(0),
            ],
            count: PREDICATE_STATE_COUNT,
        })?;
        let state = self.allocate_array_for_each_state(NativeCallState {
            values: [
                receiver,
                callback,
                Value::from_heap_ref(predicate.raw()),
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
                if self.is_object_value(value) {
                    return self.dispatch_object_primitive_conversion(
                        ConversionConsumer::ArrayLength,
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                        value,
                        site.call_site,
                    );
                }
                self.resume_array_for_each_after_length_primitive(site, state, value)
            }
            ArrayForEachStage::OutputConstructor => {
                if self.is_object_value(value) {
                    if self.is_constructor_value(value)? {
                        let constructor_realm = self.realm_for_callable(value)?;
                        if constructor_realm != self.active_realm
                            && self.realm_array_constructor(constructor_realm) == Some(value)
                        {
                            return self.finish_array_output_species(
                                site,
                                state,
                                Value::from_immediate(Immediate::Undefined),
                                false,
                            );
                        }
                    }
                    let (output, _) = self
                        .array_output_state(state)?
                        .ok_or(ExecutionError::MissingNativeContinuation)?;
                    self.set_array_for_each_value(output, OUTPUT_CONSTRUCTOR, value)?;
                    let species = self
                        .realm
                        .well_known_symbols
                        .species
                        .expect("Symbol.species initializes before Array");
                    let key = self.property_key(species)?;
                    let observed = self.dispatch_array_for_each_get(
                        site,
                        state,
                        ArrayForEachStage::OutputSpecies,
                        value,
                        key,
                    )?;
                    if let Some(observed) = observed {
                        self.resume_array_for_each(
                            site,
                            state,
                            ArrayForEachStage::OutputSpecies,
                            observed,
                            value,
                        )?;
                    }
                    Ok(())
                } else {
                    self.finish_array_output_species(site, state, value, false)
                }
            }
            ArrayForEachStage::OutputSpecies => {
                self.finish_array_output_species(site, state, value, true)
            }
            ArrayForEachStage::OutputConstruct => {
                self.finish_array_output_construct(site, state, value)
            }
            ArrayForEachStage::OutputDefine => {
                let (output, kind) = self
                    .array_output_state(state)?
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                self.finish_array_output_write(output, kind)?;
                self.advance_array_for_each(site, state)
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
                    if self.select_array_iteration_result(site, state, returned, element)? {
                        return Ok(());
                    }
                } else {
                    self.skip_array_iteration_holes(state)?;
                }
                self.advance_array_for_each(site, state)
            }
            ArrayForEachStage::Get => {
                let Some(returned) = self.call_array_for_each_callback(site, state, value)? else {
                    return Ok(());
                };
                if self.select_array_iteration_result(site, state, returned, value)? {
                    return Ok(());
                }
                self.advance_array_for_each(site, state)
            }
            ArrayForEachStage::Callback => {
                if self.select_array_iteration_result(site, state, value, retained)? {
                    return Ok(());
                }
                self.advance_array_for_each(site, state)
            }
            ArrayForEachStage::ReduceHas => self.resume_array_reduce_has(site, state, value),
            ArrayForEachStage::ReduceGet => self.resume_array_reduce_get(site, state, value),
            ArrayForEachStage::ReduceCallback => {
                self.finish_array_reduce_callback(site, state, value)
            }
            ArrayForEachStage::SearchHas => self.resume_array_search_has(site, state, value),
            ArrayForEachStage::SearchGet => self.resume_array_search_get(site, state, value),
            ArrayForEachStage::FindGet => self.resume_array_find_get(site, state, value),
            ArrayForEachStage::FindCallback => {
                self.resume_array_find_callback(site, state, value, retained)
            }
        }
    }

    /// Applies ToLength to a primitive and resumes the selected shared Array iteration contract.
    pub(crate) fn resume_array_for_each_after_length_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let guard = NativeContinuation::array_for_each(
            site,
            ArrayForEachStage::Length,
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
        if self.is_array_search_state(state)? {
            return self.begin_array_search_index(site, state);
        }
        let callback = self.native_call_state_snapshot(state)?.values[FOREACH_CALLBACK];
        self.resolve_function_object(callback)?;
        if self.is_array_find_state(state)? {
            self.begin_array_find_after_length(site, state)
        } else if self.is_array_reduce_state(state)? {
            self.advance_array_reduce(site, state)
        } else if self.array_output_state(state)?.is_some() {
            self.begin_array_output_species(site, state)
        } else {
            self.advance_array_for_each(site, state)
        }
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
                let result = if let Some((output, _)) = self.array_output_state(state)? {
                    self.native_call_state_snapshot(output)?.values[OUTPUT_RESULT]
                } else if let Some(predicate) = self.array_predicate_state(state)? {
                    self.native_call_state_snapshot(predicate)?.values
                        [PREDICATE_CONTINUE_TRUTHINESS]
                } else {
                    Value::from_immediate(Immediate::Undefined)
                };
                return self.write(site.caller_base, site.destination, result);
            }
            self.set_array_for_each_number(state, FOREACH_NEXT_INDEX, index + 1)?;
            let key = Value::from_f64(index as f64);
            let Some(has) = self.dispatch_array_iteration_has(
                site,
                state,
                ArrayForEachStage::Has,
                pending.values[FOREACH_RECEIVER],
                key,
            )?
            else {
                return Ok(());
            };
            if !self.is_truthy_value(has)? {
                self.skip_array_iteration_holes(state)?;
                continue;
            }
            let Some(element) = self.dispatch_array_for_each_element_get(site, state)? else {
                return Ok(());
            };
            let Some(returned) = self.call_array_for_each_callback(site, state, element)? else {
                return Ok(());
            };
            if self.select_array_iteration_result(site, state, returned, element)? {
                return Ok(());
            }
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
        let this_argument = if let Some((output, _)) = self.array_output_state(state)? {
            self.native_call_state_snapshot(output)?.values[OUTPUT_THIS_ARGUMENT]
        } else if let Some(predicate) = self.array_predicate_state(state)? {
            self.native_call_state_snapshot(predicate)?.values[PREDICATE_THIS_ARGUMENT]
        } else {
            pending.values[FOREACH_THIS_ARGUMENT]
        };
        self.call_array_iteration_callback(
            site,
            state,
            value,
            index,
            this_argument,
            ArrayForEachStage::Callback,
        )
    }

    /// Calls one Array predicate while preserving its state and element across suspension.
    fn call_array_iteration_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
        index: Value,
        this_argument: Value,
        stage: ArrayForEachStage,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let continuation = NativeContinuation::array_for_each(
            site,
            stage,
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
                .expect("Array iteration callback publishes one frame");
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
    pub(super) fn dispatch_array_iteration_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ArrayForEachStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_for_each_parent(site, state, stage, key)?;
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
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
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

    /// Returns the output side-state and mode for Array.prototype.filter/map.
    fn array_output_state(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<Option<(GcRef<NativeCallState>, ArrayOutputKind)>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let Some(raw) = pending.values[FOREACH_CALLBACK + 1].as_heap_ref() else {
            return Ok(None);
        };
        let Ok(output) = self
            .heap
            .checked_reference(raw, self.types.native_call_state)
        else {
            return Ok(None);
        };
        let snapshot = self.native_call_state_snapshot(output)?;
        let kind = match snapshot.count {
            FILTER_STATE_COUNT => ArrayOutputKind::Filter,
            MAP_STATE_COUNT => ArrayOutputKind::Map,
            _ => return Ok(None),
        };
        Ok(Some((output, kind)))
    }

    /// Returns the every/some side-state when the shared iteration carries a predicate contract.
    fn array_predicate_state(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<Option<GcRef<NativeCallState>>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let Some(raw) = pending.values[FOREACH_THIS_ARGUMENT].as_heap_ref() else {
            return Ok(None);
        };
        let Ok(predicate) = self
            .heap
            .checked_reference(raw, self.types.native_call_state)
        else {
            return Ok(None);
        };
        let snapshot = self.native_call_state_snapshot(predicate)?;
        Ok((snapshot.count == PREDICATE_STATE_COUNT).then_some(predicate))
    }

    /// Appends a selected value, suspending around a Proxy [[DefineOwnProperty]] trap.
    fn write_array_output_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        output: GcRef<NativeCallState>,
        kind: ArrayOutputKind,
        value: Value,
    ) -> Result<bool, ExecutionError> {
        // Keep the iteration state and selected element in VM-visible roots before atom creation.
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.set_array_for_each_value(output, OUTPUT_PENDING_VALUE, value)?;
        let index = match kind {
            ArrayOutputKind::Filter => exact_nonnegative_integer(
                self.native_call_state_snapshot(output)?.values[OUTPUT_NEXT_INDEX],
            )?,
            ArrayOutputKind::Map => {
                exact_nonnegative_integer(
                    self.native_call_state_snapshot(state)?.values[FOREACH_NEXT_INDEX],
                )? - 1
            }
        };
        let key = self.safe_integer_property_atom(index)?;
        let rooted_state = self.read(site.caller_base, site.destination)?;
        let state = self.native_call_state_reference(rooted_state)?;
        let (output, _) = self
            .array_output_state(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let pending = self.native_call_state_snapshot(output)?;
        let result = pending.values[OUTPUT_RESULT];
        let value = pending.values[OUTPUT_PENDING_VALUE];
        let descriptor = DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        if self.is_proxy_value(result) {
            self.push_array_for_each_parent(site, state, ArrayForEachStage::OutputDefine, result)?;
            let frame_depth = self.fiber.frames.len();
            if let Err(error) = self.dispatch_proxy_define(
                site,
                result,
                key.into(),
                descriptor.into(),
                ProxyDefineMode::Object,
            ) {
                self.pop_native_continuation()?;
                return Err(error);
            }
            if self.fiber.frames.len() != frame_depth {
                return Ok(true);
            }
            let continuation = self.pop_native_continuation()?;
            if continuation.kind()
                != NativeContinuationKind::ArrayForEach(ArrayForEachStage::OutputDefine)
            {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            let state = self.native_call_state_reference(continuation.first())?;
            let (output, kind) = self
                .array_output_state(state)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            self.finish_array_output_write(output, kind)?;
            return Ok(false);
        }
        self.define_data_property(result, key, descriptor)?;
        let rooted_state = self.read(site.caller_base, site.destination)?;
        let state = self.native_call_state_reference(rooted_state)?;
        let (output, kind) = self
            .array_output_state(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.finish_array_output_write(output, kind)?;
        Ok(false)
    }

    /// Advances the dense filter output cursor only after CreateDataPropertyOrThrow succeeds.
    fn finish_array_output_write(
        &mut self,
        output: GcRef<NativeCallState>,
        kind: ArrayOutputKind,
    ) -> Result<(), ExecutionError> {
        if kind == ArrayOutputKind::Map {
            return Ok(());
        }
        let index = exact_nonnegative_integer(
            self.native_call_state_snapshot(output)?.values[OUTPUT_NEXT_INDEX],
        )?;
        self.set_array_for_each_number(output, OUTPUT_NEXT_INDEX, index + 1)
    }

    /// Applies filter retention or every/some short-circuit semantics to a callback result.
    fn select_array_iteration_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        returned: Value,
        element: Value,
    ) -> Result<bool, ExecutionError> {
        if let Some((output, kind)) = self.array_output_state(state)? {
            if kind == ArrayOutputKind::Map {
                return self.write_array_output_value(site, state, output, kind, returned);
            }
            if self.is_truthy_value(returned)? {
                return self.write_array_output_value(site, state, output, kind, element);
            }
        }
        if let Some(predicate) = self.array_predicate_state(state)? {
            let expected = self.native_call_state_snapshot(predicate)?.values
                [PREDICATE_CONTINUE_TRUTHINESS]
                .as_immediate()
                == Some(Immediate::True);
            if self.is_truthy_value(returned)? != expected {
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(if expected {
                        Immediate::False
                    } else {
                        Immediate::True
                    }),
                )?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn exact_nonnegative_integer(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return Err(ExecutionError::UnsupportedNumberConversion(value));
    }
    Ok(number as u64)
}
