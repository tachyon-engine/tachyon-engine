//! Resumable fixed Number `%TypedArray.prototype.set%` implementation.

use super::*;

const SET_TARGET: usize = 0;
const SET_SOURCE: usize = 1;
const SET_OFFSET: usize = 2;
const SET_LENGTH: usize = 3;
const SET_INDEX: usize = 4;
const SET_STATE_SLOTS: u8 = 5;

struct TypedArraySetRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArraySetRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts set with an allocation-free typed-source and primitive-offset fast path.
    pub(crate) fn begin_typed_array_set(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = site.this_value;
        let _ = self.typed_array_snapshot(target)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let source = self.call_argument(site, 0)?.unwrap_or(undefined);
        let offset = self.call_argument(site, 1)?.unwrap_or(undefined);
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_typed_array_value(source) && !self.is_object_value(offset) {
            let offset = typed_array_set_offset(self.convert_to_number(offset)?)?;
            return self.finish_typed_array_set_from_typed_array(
                continuation_site,
                target,
                source,
                offset,
            );
        }
        let state = self.allocate_typed_array_set_state(NativeCallState {
            values: [target, source, offset, undefined, Value::from_i32(0)],
            count: SET_STATE_SLOTS,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.begin_typed_array_set_conversion(
            continuation_site,
            state,
            ConversionConsumer::TypedArraySetOffset,
            offset,
        )
    }

    /// Resumes one observable source length or indexed property read.
    pub(crate) fn resume_typed_array_set(
        &mut self,
        continuation: NativeContinuation,
        stage: TypedArraySetStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            TypedArraySetStage::Length => self.begin_typed_array_set_conversion(
                continuation.site(),
                state,
                ConversionConsumer::TypedArraySetLength,
                value,
            ),
            TypedArraySetStage::Element => self.begin_typed_array_set_conversion(
                continuation.site(),
                state,
                ConversionConsumer::TypedArraySetElement,
                value,
            ),
        }
    }

    /// Resumes one offset, source-length, or source-element ToNumber operation.
    pub(crate) fn resume_typed_array_set_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(value)?;
        match consumer {
            ConversionConsumer::TypedArraySetOffset => {
                let offset = typed_array_set_offset(number)?;
                self.set_typed_array_set_scalar(state, SET_OFFSET, offset as u64)?;
                self.continue_typed_array_set_after_offset(site, state)
            }
            ConversionConsumer::TypedArraySetLength => {
                let length = typed_array_to_length(number)?;
                self.finish_typed_array_set_length(site, state, length)
            }
            ConversionConsumer::TypedArraySetElement => {
                let number = numeric_value(number)
                    .ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
                self.write_typed_array_set_array_like_element(state, number)?;
                self.advance_typed_array_set_array_like(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Selects the typed-source bulk path or initializes array-like observable state.
    fn continue_typed_array_set_after_offset(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let offset = typed_array_set_usize(pending.values[SET_OFFSET])?;
        if self.is_typed_array_value(pending.values[SET_SOURCE]) {
            return self.finish_typed_array_set_from_typed_array(
                site,
                pending.values[SET_TARGET],
                pending.values[SET_SOURCE],
                offset,
            );
        }
        self.begin_typed_array_set_array_like(site, state)
    }

    /// Captures target length before ToObject and the observable source length Get.
    fn begin_typed_array_set_array_like(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let target = self.typed_array_snapshot(pending.values[SET_TARGET])?;
        self.typed_array_backing(target.buffer)?;
        self.set_typed_array_set_scalar(state, SET_LENGTH, target.length as u64)?;
        let source = self.coerce_to_object(pending.values[SET_SOURCE])?;
        let state =
            self.native_call_state_reference(self.read(site.caller_base, site.destination)?)?;
        self.set_typed_array_set_value(state, SET_SOURCE, source)?;
        let length = PropertyKey::Atom(self.length_atom()?);
        self.read_typed_array_set_property(site, state, TypedArraySetStage::Length, length)
    }

    /// Replaces targetLength with srcLength after the range check succeeds.
    fn finish_typed_array_set_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        source_length: u64,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let target_length = typed_array_set_usize(pending.values[SET_LENGTH])?;
        let offset = typed_array_set_usize(pending.values[SET_OFFSET])?;
        let source_length =
            usize::try_from(source_length).map_err(|_| ExecutionError::InvalidArrayLength)?;
        if offset > target_length || source_length > target_length - offset {
            return Err(ExecutionError::InvalidArrayLength);
        }
        self.set_typed_array_set_scalar(state, SET_LENGTH, source_length as u64)?;
        self.set_typed_array_set_scalar(state, SET_INDEX, 0)?;
        self.advance_typed_array_set_array_like(site, state)
    }

    /// Drains direct data reads iteratively and suspends only for getters or conversions.
    fn advance_typed_array_set_array_like(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        loop {
            let pending = self.native_call_state_snapshot(state)?;
            let index = typed_array_set_usize(pending.values[SET_INDEX])?;
            let length = typed_array_set_usize(pending.values[SET_LENGTH])?;
            if index == length {
                return self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            let key = PropertyKey::Atom(self.safe_integer_property_atom(index as u64)?);
            match self.resolve_property_read_until_proxy(pending.values[SET_SOURCE], key)? {
                PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                    if self.is_object_value(value) {
                        return self.begin_typed_array_set_conversion(
                            site,
                            state,
                            ConversionConsumer::TypedArraySetElement,
                            value,
                        );
                    }
                    let number = self.convert_to_number(value)?;
                    let number = numeric_value(number)
                        .ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
                    self.write_typed_array_set_array_like_element(state, number)?;
                }
                PropertyReadResolution::Read(PropertyRead::Missing) => {
                    let number = f64::NAN;
                    self.write_typed_array_set_array_like_element(state, number)?;
                }
                PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    let number = f64::NAN;
                    self.write_typed_array_set_array_like_element(state, number)?;
                }
                PropertyReadResolution::Read(PropertyRead::Accessor(_))
                | PropertyReadResolution::Proxy(_) => {
                    return self.read_typed_array_set_property(
                        site,
                        state,
                        TypedArraySetStage::Element,
                        key,
                    );
                }
            }
        }
    }

    /// Writes one converted value or silently skips it after mid-iteration detach.
    fn write_typed_array_set_array_like_element(
        &mut self,
        state: GcRef<NativeCallState>,
        number: f64,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let offset = typed_array_set_usize(pending.values[SET_OFFSET])?;
        let index = typed_array_set_usize(pending.values[SET_INDEX])?;
        let target = self.typed_array_snapshot(pending.values[SET_TARGET])?;
        let target_index = offset
            .checked_add(index)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        match self.typed_array_write_element(target, target_index, number) {
            Ok(()) | Err(ExecutionError::DetachedArrayBuffer) => {}
            Err(error) => return Err(error),
        }
        self.set_typed_array_set_scalar(state, SET_INDEX, (index + 1) as u64)
    }

    /// Copies one typed source with raw same-kind and numeric cross-kind semantics.
    fn finish_typed_array_set_from_typed_array(
        &mut self,
        site: NativeContinuationSite,
        target_value: Value,
        source_value: Value,
        offset: usize,
    ) -> Result<(), ExecutionError> {
        let target = self.typed_array_snapshot(target_value)?;
        self.typed_array_backing(target.buffer)?;
        let source = self.typed_array_snapshot(source_value)?;
        self.typed_array_backing(source.buffer)?;
        if offset > target.length || source.length > target.length - offset {
            return Err(ExecutionError::InvalidArrayLength);
        }
        if source.kind == target.kind && source.buffer == target.buffer {
            self.copy_typed_array_set_same_backing(target, source, offset)?;
        } else {
            let source_bytes = self.snapshot_typed_array_set_source(source)?;
            let bytes = if source.kind == target.kind {
                source_bytes
            } else {
                convert_typed_array_set_bytes(source, target.kind, &source_bytes)?
            };
            self.write_typed_array_set_bytes(target, offset, &bytes)?;
        }
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Uses overlap-safe copyWithin when source and target share one fixed backing.
    fn copy_typed_array_set_same_backing(
        &mut self,
        target: TypedArraySnapshot,
        source: TypedArraySnapshot,
        offset: usize,
    ) -> Result<(), ExecutionError> {
        let width = target.kind.byte_width();
        let byte_count = source
            .length
            .checked_mul(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let source_end = source
            .byte_offset
            .checked_add(byte_count)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let target_start = target
            .byte_offset
            .checked_add(
                offset
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let target_end = target_start
            .checked_add(byte_count)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let data = self.typed_array_backing(target.buffer)?;
        self.with_buffer_backing_bytes_mut(&data, |data, visible| {
            if source_end > visible
                || target_end > visible
                || source_end > data.len()
                || target_end > data.len()
            {
                return Err(ExecutionError::InvalidArrayLength);
            }
            data.copy_within(source.byte_offset..source_end, target_start);
            Ok(())
        })
    }

    /// Copies one checked source range into exact-capacity temporary byte storage.
    fn snapshot_typed_array_set_source(
        &mut self,
        source: TypedArraySnapshot,
    ) -> Result<Vec<u8>, ExecutionError> {
        let byte_count = source
            .length
            .checked_mul(source.kind.byte_width())
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = source
            .byte_offset
            .checked_add(byte_count)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let data = self.typed_array_backing(source.buffer)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| ExecutionError::TypedArraySetAllocationFailed)?;
        self.with_buffer_backing_bytes(&data, |data, visible| {
            if end > visible || end > data.len() {
                return Err(ExecutionError::InvalidArrayLength);
            }
            bytes.extend_from_slice(&data[source.byte_offset..end]);
            Ok(())
        })?;
        Ok(bytes)
    }

    /// Publishes one exact converted/raw byte snapshot into the target range.
    fn write_typed_array_set_bytes(
        &mut self,
        target: TypedArraySnapshot,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), ExecutionError> {
        let start = target
            .byte_offset
            .checked_add(
                offset
                    .checked_mul(target.kind.byte_width())
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = start
            .checked_add(bytes.len())
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let data = self.typed_array_backing(target.buffer)?;
        self.with_buffer_backing_bytes_mut(&data, |data, visible| {
            if end > visible || end > data.len() {
                return Err(ExecutionError::InvalidArrayLength);
            }
            data[start..end].copy_from_slice(bytes);
            Ok(())
        })
    }

    /// Dispatches ToPrimitive only when a set operand is an object.
    fn begin_typed_array_set_conversion(
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
        self.resume_typed_array_set_conversion(site, state, consumer, value)
    }

    /// Reads a source property and resumes immediately when no JavaScript frame was entered.
    fn read_typed_array_set_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArraySetStage,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[SET_SOURCE];
        let depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::typed_array_set(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                receiver,
            ))
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth || self.fiber.completions.len() == depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_typed_array_set(continuation, stage, value)
    }

    /// Allocates fixed traced set state under the complete VM root set.
    fn allocate_typed_array_set_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArraySetRoots {
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

    /// Replaces one traced set-state value and publishes its generational edge.
    fn set_typed_array_set_value(
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

    /// Stores one exact non-negative scalar in a fixed Value slot.
    fn set_typed_array_set_scalar(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: u64,
    ) -> Result<(), ExecutionError> {
        self.set_typed_array_set_value(state, slot, Value::from_f64(value as f64))
    }
}

/// Converts an offset using ToIntegerOrInfinity and rejects negative or infinite results.
#[inline(always)]
fn typed_array_set_offset(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    let integer = if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    };
    if !integer.is_finite() || integer < 0.0 || integer > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(integer as usize)
}

#[inline(always)]
fn typed_array_set_usize(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}

/// Converts an exact source snapshot into one exact-capacity target byte buffer.
fn convert_typed_array_set_bytes(
    source: TypedArraySnapshot,
    target_kind: TypedArrayKind,
    source_bytes: &[u8],
) -> Result<Vec<u8>, ExecutionError> {
    let target_length = source
        .length
        .checked_mul(target_kind.byte_width())
        .ok_or(ExecutionError::InvalidArrayLength)?;
    let mut target = Vec::new();
    target
        .try_reserve_exact(target_length)
        .map_err(|_| ExecutionError::TypedArraySetAllocationFailed)?;
    target.resize(target_length, 0);
    for index in 0..source.length {
        let source_start = index * source.kind.byte_width();
        let mut raw = [0_u8; 8];
        raw[..source.kind.byte_width()]
            .copy_from_slice(&source_bytes[source_start..source_start + source.kind.byte_width()]);
        let value = data_view_decode(data_view_kind(source.kind)?, raw, true);
        let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
        let encoded = typed_array_set_encode(target_kind, number)?;
        let target_start = index * target_kind.byte_width();
        target[target_start..target_start + target_kind.byte_width()]
            .copy_from_slice(&encoded[..target_kind.byte_width()]);
    }
    Ok(target)
}

#[inline(always)]
fn typed_array_set_encode(kind: TypedArrayKind, number: f64) -> Result<[u8; 8], ExecutionError> {
    if kind == TypedArrayKind::Uint8Clamped {
        let mut bytes = [0_u8; 8];
        bytes[0] = to_uint8_clamp(number);
        Ok(bytes)
    } else {
        Ok(data_view_encode(data_view_kind(kind)?, number, true))
    }
}
