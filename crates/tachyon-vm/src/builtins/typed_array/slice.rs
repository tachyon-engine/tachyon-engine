//! Resumable fixed Number `%TypedArray.prototype.slice%`.

use super::*;

const SLICE_SOURCE: usize = 0;
const SLICE_START: usize = 1;
const SLICE_END: usize = 2;
const SLICE_AUXILIARY: usize = 3;
const SLICE_RESULT: usize = 4;
const SLICE_STATE_SLOTS: u8 = 5;

struct TypedArraySliceRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArraySliceRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the source and starts ordered start/end conversion from an internal length snapshot.
    pub(crate) fn begin_typed_array_slice(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let source = site.this_value;
        let snapshot = self.typed_array_snapshot(source)?;
        self.typed_array_backing(snapshot.buffer)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let start = self.call_argument(site, 0)?.unwrap_or(undefined);
        let end = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_typed_array_slice_state(NativeCallState {
            values: [
                source,
                start,
                end,
                Value::from_f64(snapshot.length as f64),
                undefined,
            ],
            count: SLICE_STATE_SLOTS,
        })?;
        let site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_typed_array_slice_state(site, state)?;
        self.begin_typed_array_slice_conversion(
            site,
            state,
            ConversionConsumer::TypedArraySliceStart,
            start,
        )
    }

    /// Resumes start/end ToIntegerOrInfinity after observable object conversion.
    pub(crate) fn resume_typed_array_slice_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::TypedArraySliceStart => {
                self.finish_typed_array_slice_start(site, state, value)
            }
            ConversionConsumer::TypedArraySliceEnd => {
                self.finish_typed_array_slice_end(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Routes constructor/species Gets and custom construction back into the slice state machine.
    pub(crate) fn resume_typed_array_slice(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArraySliceStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_typed_array_slice_state(site, state)?;
        match stage {
            TypedArraySliceStage::Constructor => {
                self.resume_typed_array_slice_constructor(site, state, value)
            }
            TypedArraySliceStage::Species => {
                self.finish_typed_array_slice_species(site, state, value, true)
            }
            TypedArraySliceStage::Construct => {
                self.finish_typed_array_slice_construct(site, state, value)
            }
        }
    }

    /// Clamps start against the initial length and then observes end.
    fn finish_typed_array_slice_start(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = typed_array_slice_usize(pending.values[SLICE_AUXILIARY])?;
        let start = relative_typed_array_slice_index(
            length,
            typed_array_slice_integer(self.convert_to_number(value)?)?,
        );
        self.set_typed_array_slice_value(state, SLICE_START, Value::from_f64(start as f64))?;
        let end = pending.values[SLICE_END];
        if end.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_typed_array_slice_indices(site, state, length);
        }
        self.begin_typed_array_slice_conversion(
            site,
            state,
            ConversionConsumer::TypedArraySliceEnd,
            end,
        )
    }

    /// Clamps explicit end and freezes the requested result count before species lookup.
    fn finish_typed_array_slice_end(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = typed_array_slice_usize(pending.values[SLICE_AUXILIARY])?;
        let end = relative_typed_array_slice_index(
            length,
            typed_array_slice_integer(self.convert_to_number(value)?)?,
        );
        self.finish_typed_array_slice_indices(site, state, end)
    }

    /// Stores count and starts the observable SpeciesConstructor lookup on the source.
    fn finish_typed_array_slice_indices(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        end: usize,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let start = typed_array_slice_usize(pending.values[SLICE_START])?;
        self.set_typed_array_slice_value(
            state,
            SLICE_END,
            Value::from_f64(end.saturating_sub(start) as f64),
        )?;
        let constructor = self.constructor_atom()?;
        if let Some(value) = self.dispatch_typed_array_slice_get(
            site,
            state,
            TypedArraySliceStage::Constructor,
            pending.values[SLICE_SOURCE],
            constructor.into(),
        )? {
            self.resume_typed_array_slice_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Validates constructor shape and observes its realm-correct `@@species` property.
    fn resume_typed_array_slice_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_typed_array_slice_species(site, state, constructor, false);
        }
        if !self.is_object_value(constructor) {
            return Err(ExecutionError::NotObject(constructor));
        }
        let constructor_realm = if self.is_constructor_value(constructor)? {
            Some(self.realm_for_callable(constructor)?)
        } else {
            None
        };
        self.set_typed_array_slice_value(state, SLICE_AUXILIARY, constructor)?;
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
            .expect("Symbol.species initializes before TypedArray slice");
        let species = self.property_key(species)?;
        if let Some(value) = self.dispatch_typed_array_slice_get(
            site,
            state,
            TypedArraySliceStage::Species,
            constructor,
            species,
        )? {
            self.finish_typed_array_slice_species(site, state, value, true)?;
        }
        Ok(())
    }

    /// Selects the source-kind intrinsic fallback or constructs the observed species.
    fn finish_typed_array_slice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        observed: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        let constructor = if observed.as_immediate() == Some(Immediate::Undefined)
            || (from_species && observed.as_immediate() == Some(Immediate::Null))
        {
            let source = self.native_call_state_snapshot(state)?.values[SLICE_SOURCE];
            let kind = self.typed_array_snapshot(source)?.kind;
            self.realm.typed_array_constructors[kind.index()]
                .expect("concrete TypedArray constructor initializes before slice")
        } else {
            observed
        };
        if !self.is_constructor_value(constructor)? {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_typed_array_slice_result(site, state, constructor)
    }

    /// Roots one exact count argument while the chosen species constructor executes.
    fn construct_typed_array_slice_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_typed_array_slice_value(state, SLICE_AUXILIARY, constructor)?;
        let count = self.native_call_state_snapshot(state)?.values[SLICE_END];
        let undefined = Value::from_immediate(Immediate::Undefined);
        self.push_typed_array_slice_parent(
            site,
            state,
            TypedArraySliceStage::Construct,
            constructor,
        )?;
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, vec![count]) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_typed_array_slice_parent(
            site,
            state,
            TypedArraySliceStage::Construct,
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
                .expect("TypedArray species constructor publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_typed_array_slice_construct(site, state, result)
    }

    /// Validates the species result and copies only after the required source revalidation.
    fn finish_typed_array_slice_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let target = self.typed_array_snapshot(result)?;
        self.typed_array_backing(target.buffer)?;
        let pending = self.native_call_state_snapshot(state)?;
        let requested = typed_array_slice_usize(pending.values[SLICE_END])?;
        if target.length < requested {
            return Err(ExecutionError::TypedArraySpeciesResultTooShort);
        }
        self.set_typed_array_slice_value(state, SLICE_RESULT, result)?;
        if requested == 0 {
            return self.write(site.caller_base, site.destination, result);
        }
        let source_value = pending.values[SLICE_SOURCE];
        let source = self.typed_array_snapshot(source_value)?;
        self.typed_array_backing(source.buffer)?;
        let start = typed_array_slice_usize(pending.values[SLICE_START])?;
        let count = requested.min(source.length.saturating_sub(start));
        if source.kind == target.kind {
            self.copy_typed_array_slice_same_kind(source, target, start, count)?;
        } else {
            self.copy_typed_array_slice_cross_kind(source, target, start, count)?;
        }
        self.write(site.caller_base, site.destination, result)
    }

    /// Copies raw bytes while preserving both NaN payloads and same-buffer forward overlap.
    fn copy_typed_array_slice_same_kind(
        &mut self,
        source: TypedArraySnapshot,
        target: TypedArraySnapshot,
        start: usize,
        count: usize,
    ) -> Result<(), ExecutionError> {
        let width = source.kind.byte_width();
        let byte_count = count
            .checked_mul(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let source_start = source
            .byte_offset
            .checked_add(
                start
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        if source.buffer == target.buffer {
            return self.copy_overlapping_typed_array_slice_bytes(
                source.buffer,
                source_start,
                target.byte_offset,
                byte_count,
            );
        }
        let source_data = self.typed_array_backing(source.buffer)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(source_data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let end = source_start
                    .checked_add(byte_count)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                bytes.extend_from_slice(
                    data.bytes
                        .get(source_start..end)
                        .ok_or(ExecutionError::InvalidArrayLength)?,
                );
                Ok::<(), ExecutionError>(())
            })
        })?;
        let target_data = self.typed_array_backing(target.buffer)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(target_data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let end = target
                    .byte_offset
                    .checked_add(byte_count)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                data.bytes
                    .get_mut(target.byte_offset..end)
                    .ok_or(ExecutionError::InvalidArrayLength)?
                    .copy_from_slice(&bytes);
                Ok(())
            })
        })
    }

    /// Performs the spec's observable byte-by-byte forward copy within one backing store.
    fn copy_overlapping_typed_array_slice_bytes(
        &mut self,
        buffer: Value,
        source_start: usize,
        target_start: usize,
        byte_count: usize,
    ) -> Result<(), ExecutionError> {
        let data = self.typed_array_backing(buffer)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let source_end = source_start
                    .checked_add(byte_count)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                let target_end = target_start
                    .checked_add(byte_count)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                if source_end > data.bytes.len() || target_end > data.bytes.len() {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                for offset in 0..byte_count {
                    data.bytes[target_start + offset] = data.bytes[source_start + offset];
                }
                Ok(())
            })
        })
    }

    /// Converts different Number element kinds iteratively with integer-indexed semantics.
    fn copy_typed_array_slice_cross_kind(
        &mut self,
        source: TypedArraySnapshot,
        target: TypedArraySnapshot,
        start: usize,
        count: usize,
    ) -> Result<(), ExecutionError> {
        for index in 0..count {
            let value = self.typed_array_read_element(source, start + index)?;
            let number = numeric_value(value)
                .expect("fixed Number TypedArray decoding always returns Number");
            self.typed_array_write_element(target, index, number)?;
        }
        Ok(())
    }

    /// Converts one primitive immediately or dispatches observable numeric conversion.
    fn begin_typed_array_slice_conversion(
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
        self.resume_typed_array_slice_conversion(site, state, consumer, value)
    }

    /// Dispatches a Proxy/accessor-aware constructor or species property read.
    fn dispatch_typed_array_slice_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArraySliceStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_typed_array_slice_parent(site, state, stage, receiver)?;
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

    /// Pushes the typed parent used by property callbacks and species construction.
    fn push_typed_array_slice_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArraySliceStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::typed_array_slice(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Allocates one fixed five-slot state under all VM roots.
    fn allocate_typed_array_slice_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArraySliceRoots {
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

    /// Keeps the moving state rooted in the caller destination.
    #[inline(always)]
    fn root_typed_array_slice_state(
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
    fn set_typed_array_slice_value(
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
fn typed_array_slice_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    })
}

#[inline(always)]
fn relative_typed_array_slice_index(length: usize, relative: f64) -> usize {
    if relative == f64::NEG_INFINITY {
        return 0;
    }
    if relative < 0.0 {
        return (length as f64 + relative).max(0.0) as usize;
    }
    relative.min(length as f64) as usize
}

#[inline(always)]
fn typed_array_slice_usize(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}
