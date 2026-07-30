//! Resumable fixed Number `%TypedArray.prototype.subarray%`.

use super::*;

const SUBARRAY_SOURCE: usize = 0;
const SUBARRAY_BUFFER: usize = 1;
const SUBARRAY_START: usize = 2;
const SUBARRAY_END: usize = 3;
const SUBARRAY_AUXILIARY: usize = 4;
const SUBARRAY_STATE_SLOTS: u8 = 5;

struct TypedArraySubarrayRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArraySubarrayRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Captures fixed view metadata without rejecting a detached source before argument coercion.
    pub(crate) fn begin_typed_array_subarray(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let source = site.this_value;
        let snapshot = self.typed_array_snapshot(source)?;
        let initial_length = match self.typed_array_backing(snapshot.buffer) {
            Ok(_) => snapshot.length,
            Err(ExecutionError::DetachedArrayBuffer) => 0,
            Err(error) => return Err(error),
        };
        let undefined = Value::from_immediate(Immediate::Undefined);
        let start = self.call_argument(site, 0)?.unwrap_or(undefined);
        let end = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_typed_array_subarray_state(NativeCallState {
            values: [
                source,
                snapshot.buffer,
                start,
                end,
                Value::from_f64(initial_length as f64),
            ],
            count: SUBARRAY_STATE_SLOTS,
        })?;
        let site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_typed_array_subarray_state(site, state)?;
        self.begin_typed_array_subarray_conversion(
            site,
            state,
            ConversionConsumer::TypedArraySubarrayStart,
            start,
        )
    }

    /// Resumes ordered begin/end ToIntegerOrInfinity conversions.
    pub(crate) fn resume_typed_array_subarray_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::TypedArraySubarrayStart => {
                self.finish_typed_array_subarray_start(site, state, value)
            }
            ConversionConsumer::TypedArraySubarrayEnd => {
                self.finish_typed_array_subarray_end(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Routes species property access and construction back into the state machine.
    pub(crate) fn resume_typed_array_subarray(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArraySubarrayStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_typed_array_subarray_state(site, state)?;
        match stage {
            TypedArraySubarrayStage::Constructor => {
                self.resume_typed_array_subarray_constructor(site, state, value)
            }
            TypedArraySubarrayStage::Species => {
                self.finish_typed_array_subarray_species(site, state, value, true)
            }
            TypedArraySubarrayStage::Construct => {
                self.finish_typed_array_subarray_construct(site, value)
            }
        }
    }

    /// Freezes normalized start against the initial witness length before observing end.
    fn finish_typed_array_subarray_start(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = typed_array_subarray_usize(pending.values[SUBARRAY_AUXILIARY])?;
        let start = relative_typed_array_subarray_index(
            length,
            typed_array_subarray_integer(self.convert_to_number(value)?)?,
        );
        self.set_typed_array_subarray_value(state, SUBARRAY_START, Value::from_f64(start as f64))?;
        let end = pending.values[SUBARRAY_END];
        if end.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_typed_array_subarray_indices(site, state, length);
        }
        self.begin_typed_array_subarray_conversion(
            site,
            state,
            ConversionConsumer::TypedArraySubarrayEnd,
            end,
        )
    }

    /// Freezes normalized end against the same initial witness length.
    fn finish_typed_array_subarray_end(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = typed_array_subarray_usize(pending.values[SUBARRAY_AUXILIARY])?;
        let end = relative_typed_array_subarray_index(
            length,
            typed_array_subarray_integer(self.convert_to_number(value)?)?,
        );
        self.finish_typed_array_subarray_indices(site, state, end)
    }

    /// Saves either a fixed result count or the omitted-length tracking marker.
    fn finish_typed_array_subarray_indices(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        end: usize,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let start = typed_array_subarray_usize(pending.values[SUBARRAY_START])?;
        let tracking_omitted = pending.values[SUBARRAY_END].as_immediate()
            == Some(Immediate::Undefined)
            && self.typed_array_length_mode(pending.values[SUBARRAY_SOURCE])?
                == ViewLengthMode::Tracking;
        self.set_typed_array_subarray_value(
            state,
            SUBARRAY_END,
            if tracking_omitted {
                Value::from_immediate(Immediate::Undefined)
            } else {
                Value::from_f64(end.saturating_sub(start) as f64)
            },
        )?;
        let constructor = self.constructor_atom()?;
        if let Some(value) = self.dispatch_typed_array_subarray_get(
            site,
            state,
            TypedArraySubarrayStage::Constructor,
            pending.values[SUBARRAY_SOURCE],
            constructor.into(),
        )? {
            self.resume_typed_array_subarray_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Applies SpeciesConstructor rules using the observed constructor Realm's species symbol.
    fn resume_typed_array_subarray_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_typed_array_subarray_species(site, state, constructor, false);
        }
        if !self.is_object_value(constructor) {
            return Err(ExecutionError::NotObject(constructor));
        }
        let constructor_realm = if self.is_constructor_value(constructor)? {
            Some(self.realm_for_callable(constructor)?)
        } else {
            None
        };
        self.set_typed_array_subarray_value(state, SUBARRAY_AUXILIARY, constructor)?;
        let species = constructor_realm
            .and_then(|realm| {
                if realm == self.active_realm {
                    self.realm.well_known_symbols.species
                } else {
                    self.inactive_realms
                        .iter()
                        .find(|(id, _)| *id == realm)
                        .and_then(|(_, realm)| realm.well_known_symbols.species)
                }
            })
            .or(self.realm.well_known_symbols.species)
            .expect("Symbol.species initializes before TypedArray subarray");
        let species = self.property_key(species)?;
        if let Some(value) = self.dispatch_typed_array_subarray_get(
            site,
            state,
            TypedArraySubarrayStage::Species,
            constructor,
            species,
        )? {
            self.finish_typed_array_subarray_species(site, state, value, true)?;
        }
        Ok(())
    }

    /// Selects the source-kind intrinsic fallback or validates the custom species constructor.
    fn finish_typed_array_subarray_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        observed: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        let constructor = if observed.as_immediate() == Some(Immediate::Undefined)
            || (from_species && observed.as_immediate() == Some(Immediate::Null))
        {
            let source = self.native_call_state_snapshot(state)?.values[SUBARRAY_SOURCE];
            let kind = self.typed_array_snapshot(source)?.kind;
            self.realm.typed_array_constructors[kind.index()]
                .expect("concrete TypedArray constructor initializes before subarray")
        } else {
            observed
        };
        if !self.is_constructor_value(constructor)? {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_typed_array_subarray_result(site, state, constructor)
    }

    /// Constructs a tracking two-argument or fixed three-argument species result.
    fn construct_typed_array_subarray_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_typed_array_subarray_value(state, SUBARRAY_AUXILIARY, constructor)?;
        let pending = self.native_call_state_snapshot(state)?;
        let source = self.typed_array_snapshot(pending.values[SUBARRAY_SOURCE])?;
        let start = typed_array_subarray_usize(pending.values[SUBARRAY_START])?;
        let byte_offset = source
            .byte_offset
            .checked_add(
                start
                    .checked_mul(source.kind.byte_width())
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let tracking = pending.values[SUBARRAY_END].as_immediate() == Some(Immediate::Undefined);
        let argument_count = if tracking { 2 } else { 3 };
        let mut arguments = Vec::with_capacity(argument_count);
        arguments.push(pending.values[SUBARRAY_BUFFER]);
        arguments.push(Value::from_f64(byte_offset as f64));
        if !tracking {
            arguments.push(pending.values[SUBARRAY_END]);
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        self.push_typed_array_subarray_parent(
            site,
            state,
            TypedArraySubarrayStage::Construct,
            constructor,
        )?;
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, arguments) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_typed_array_subarray_parent(
            site,
            state,
            TypedArraySubarrayStage::Construct,
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
            argument_prefix_count: argument_count as u32,
            argument_count: argument_count as u32,
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
                .expect("TypedArray species constructor publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_typed_array_subarray_construct(site, result)
    }

    /// Validates only the TypedArray and attached backing required by TypedArrayCreate.
    fn finish_typed_array_subarray_construct(
        &mut self,
        site: NativeContinuationSite,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let target = self.typed_array_snapshot(result)?;
        self.typed_array_backing(target.buffer)?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Converts one primitive immediately or dispatches observable numeric conversion.
    fn begin_typed_array_subarray_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                consumer,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.resume_typed_array_subarray_conversion(site, state, consumer, value)
    }

    /// Dispatches one Proxy/accessor-aware constructor or species property read.
    fn dispatch_typed_array_subarray_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArraySubarrayStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_typed_array_subarray_parent(site, state, stage, receiver)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(None);
        }
        self.pop_native_continuation()?;
        self.read(site.caller_base, site.destination).map(Some)
    }

    /// Pushes the typed parent used by property callbacks and construction.
    fn push_typed_array_subarray_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArraySubarrayStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::typed_array_subarray(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Allocates one fixed five-slot state under all VM roots.
    fn allocate_typed_array_subarray_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArraySubarrayRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
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

    /// Keeps the moving state rooted in the caller destination.
    #[inline(always)]
    fn root_typed_array_subarray_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Replaces one traced state slot and publishes its generational edge.
    fn set_typed_array_subarray_value(
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
                Ok(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }
}

#[inline(always)]
fn typed_array_subarray_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    })
}

#[inline(always)]
fn relative_typed_array_subarray_index(length: usize, relative: f64) -> usize {
    if relative == f64::NEG_INFINITY {
        return 0;
    }
    if relative < 0.0 {
        return (length as f64 + relative).max(0.0) as usize;
    }
    relative.min(length as f64) as usize
}

#[inline(always)]
fn typed_array_subarray_usize(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}
