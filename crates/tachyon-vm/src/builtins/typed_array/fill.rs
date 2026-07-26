//! Resumable fixed Number `%TypedArray.prototype.fill%` implementation.

use super::*;

const FILL_RECEIVER: usize = 0;
const FILL_VALUE: usize = 1;
const FILL_START: usize = 2;
const FILL_END: usize = 3;
const FILL_LENGTH: usize = 4;
const FILL_STATE_SLOTS: u8 = 5;

struct TypedArrayFillRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArrayFillRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the receiver and keeps the all-primitive fill path allocation-free.
    pub(crate) fn begin_typed_array_fill(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let snapshot = self.typed_array_snapshot(receiver)?;
        self.typed_array_backing(snapshot.buffer)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let value = self.call_argument(site, 0)?.unwrap_or(undefined);
        let start = self.call_argument(site, 1)?.unwrap_or(undefined);
        let end = self.call_argument(site, 2)?.unwrap_or(undefined);
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if !self.is_object_value(value)
            && !self.is_object_value(start)
            && !self.is_object_value(end)
        {
            return self.finish_primitive_typed_array_fill(
                continuation_site,
                receiver,
                snapshot.length,
                value,
                start,
                end,
            );
        }
        let state = self.allocate_typed_array_fill_state(NativeCallState {
            values: [
                receiver,
                value,
                start,
                end,
                Value::from_f64(snapshot.length as f64),
            ],
            count: FILL_STATE_SLOTS,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.convert_typed_array_fill_value(
            continuation_site,
            state,
            ConversionConsumer::TypedArrayFillValue,
            value,
        )
    }

    /// Routes one completed ToPrimitive operation through value, start, and end order.
    pub(crate) fn resume_typed_array_fill_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::TypedArrayFillValue => {
                let number = self.convert_to_number(value)?;
                self.set_typed_array_fill_value(state, FILL_VALUE, number)?;
                let start = self.native_call_state_snapshot(state)?.values[FILL_START];
                self.convert_typed_array_fill_value(
                    site,
                    state,
                    ConversionConsumer::TypedArrayFillStart,
                    start,
                )
            }
            ConversionConsumer::TypedArrayFillStart => {
                let relative = typed_array_fill_integer(self.convert_to_number(value)?)?;
                let pending = self.native_call_state_snapshot(state)?;
                let length = typed_array_fill_usize(pending.values[FILL_LENGTH])?;
                let start = typed_array_fill_relative_index(relative, length);
                self.set_typed_array_fill_value(state, FILL_START, Value::from_f64(start as f64))?;
                if pending.values[FILL_END].as_immediate() == Some(Immediate::Undefined) {
                    self.set_typed_array_fill_value(
                        state,
                        FILL_END,
                        Value::from_f64(length as f64),
                    )?;
                    return self.finish_typed_array_fill_state(site, state);
                }
                self.convert_typed_array_fill_value(
                    site,
                    state,
                    ConversionConsumer::TypedArrayFillEnd,
                    pending.values[FILL_END],
                )
            }
            ConversionConsumer::TypedArrayFillEnd => {
                let relative = typed_array_fill_integer(self.convert_to_number(value)?)?;
                let length = typed_array_fill_usize(
                    self.native_call_state_snapshot(state)?.values[FILL_LENGTH],
                )?;
                let end = typed_array_fill_relative_index(relative, length);
                self.set_typed_array_fill_value(state, FILL_END, Value::from_f64(end as f64))?;
                self.finish_typed_array_fill_state(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts primitive inputs in specification order before touching the backing again.
    fn finish_primitive_typed_array_fill(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        length: usize,
        value: Value,
        start: Value,
        end: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let relative_start = typed_array_fill_integer(self.convert_to_number(start)?)?;
        let start = typed_array_fill_relative_index(relative_start, length);
        let end = if end.as_immediate() == Some(Immediate::Undefined) {
            length
        } else {
            let relative_end = typed_array_fill_integer(self.convert_to_number(end)?)?;
            typed_array_fill_relative_index(relative_end, length)
        };
        self.fill_typed_array_range(receiver, number, start, end)?;
        self.write(site.caller_base, site.destination, receiver)
    }

    /// Finishes a rooted observable path after all three conversions have completed.
    fn finish_typed_array_fill_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let receiver = pending.values[FILL_RECEIVER];
        let number = numeric_value(pending.values[FILL_VALUE]).ok_or(
            ExecutionError::UnsupportedNumberConversion(pending.values[FILL_VALUE]),
        )?;
        let start = typed_array_fill_usize(pending.values[FILL_START])?;
        let end = typed_array_fill_usize(pending.values[FILL_END])?;
        self.fill_typed_array_range(receiver, number, start, end)?;
        self.write(site.caller_base, site.destination, receiver)
    }

    /// Revalidates the current backing, then writes encoded elements under one no-GC borrow.
    fn fill_typed_array_range(
        &mut self,
        receiver: Value,
        number: f64,
        start: usize,
        end: usize,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.typed_array_snapshot(receiver)?;
        let data = self.typed_array_backing(snapshot.buffer)?;
        let end = end.min(snapshot.length);
        if start >= end {
            return Ok(());
        }
        let width = snapshot.kind.byte_width();
        let byte_start = snapshot
            .byte_offset
            .checked_add(
                start
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let byte_end = snapshot
            .byte_offset
            .checked_add(
                end.checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let bytes = if snapshot.kind == TypedArrayKind::Uint8Clamped {
            let mut bytes = [0_u8; 8];
            bytes[0] = to_uint8_clamp(number);
            bytes
        } else {
            data_view_encode(data_view_kind(snapshot.kind), number, true)
        };
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if byte_end > data.byte_length || byte_end > data.bytes.len() {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                for chunk in data.bytes[byte_start..byte_end].chunks_exact_mut(width) {
                    chunk.copy_from_slice(&bytes[..width]);
                }
                Ok(())
            })
        })
    }

    /// Dispatches one object conversion while the complete fixed state remains traced.
    fn convert_typed_array_fill_value(
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
        self.resume_typed_array_fill_conversion(site, state, consumer, value)
    }

    /// Updates one traced state slot and publishes its generational edge.
    fn set_typed_array_fill_value(
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

    /// Allocates observable fill state under the complete VM root set.
    fn allocate_typed_array_fill_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArrayFillRoots {
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
}

#[inline(always)]
fn typed_array_fill_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    })
}

#[inline(always)]
fn typed_array_fill_relative_index(relative: f64, length: usize) -> usize {
    if relative == f64::NEG_INFINITY {
        return 0;
    }
    if relative < 0.0 {
        return length.saturating_sub((-relative).min(length as f64) as usize);
    }
    if !relative.is_finite() || relative >= length as f64 {
        return length;
    }
    relative as usize
}

#[inline(always)]
fn typed_array_fill_usize(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}
