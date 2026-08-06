//! Resumable fixed Number `%TypedArray.prototype.copyWithin%` implementation.

use super::*;

const COPY_RECEIVER: usize = 0;
const COPY_TARGET: usize = 1;
const COPY_START: usize = 2;
const COPY_END: usize = 3;
const COPY_LENGTH: usize = 4;
const COPY_STATE_SLOTS: u8 = 5;

struct TypedArrayCopyWithinRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArrayCopyWithinRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the receiver and keeps the all-primitive argument path allocation-free.
    pub(crate) fn begin_typed_array_copy_within(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let snapshot = self.validated_typed_array_snapshot(receiver)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let target = self.call_argument(site, 0)?.unwrap_or(undefined);
        let start = self.call_argument(site, 1)?.unwrap_or(undefined);
        let end = self.call_argument(site, 2)?.unwrap_or(undefined);
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if !self.is_object_value(target)
            && !self.is_object_value(start)
            && !self.is_object_value(end)
        {
            return self.finish_primitive_typed_array_copy_within(
                continuation_site,
                receiver,
                snapshot.length,
                target,
                start,
                end,
            );
        }
        let state = self.allocate_typed_array_copy_within_state(NativeCallState {
            values: [
                receiver,
                target,
                start,
                end,
                Value::from_f64(snapshot.length as f64),
            ],
            count: COPY_STATE_SLOTS,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.convert_typed_array_copy_within_index(
            continuation_site,
            state,
            ConversionConsumer::TypedArrayCopyWithinTarget,
            target,
        )
    }

    /// Routes one completed ToPrimitive operation through target, start, and end order.
    pub(crate) fn resume_typed_array_copy_within_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let relative = typed_array_copy_integer(self.convert_to_number(value)?)?;
        let pending = self.native_call_state_snapshot(state)?;
        let length = typed_array_copy_usize(pending.values[COPY_LENGTH])?;
        let index = typed_array_copy_relative_index(relative, length);
        match consumer {
            ConversionConsumer::TypedArrayCopyWithinTarget => {
                self.set_typed_array_copy_within_value(
                    state,
                    COPY_TARGET,
                    Value::from_f64(index as f64),
                )?;
                self.convert_typed_array_copy_within_index(
                    site,
                    state,
                    ConversionConsumer::TypedArrayCopyWithinStart,
                    pending.values[COPY_START],
                )
            }
            ConversionConsumer::TypedArrayCopyWithinStart => {
                self.set_typed_array_copy_within_value(
                    state,
                    COPY_START,
                    Value::from_f64(index as f64),
                )?;
                if pending.values[COPY_END].as_immediate() == Some(Immediate::Undefined) {
                    self.set_typed_array_copy_within_value(
                        state,
                        COPY_END,
                        Value::from_f64(length as f64),
                    )?;
                    return self.finish_typed_array_copy_within_state(site, state);
                }
                self.convert_typed_array_copy_within_index(
                    site,
                    state,
                    ConversionConsumer::TypedArrayCopyWithinEnd,
                    pending.values[COPY_END],
                )
            }
            ConversionConsumer::TypedArrayCopyWithinEnd => {
                self.set_typed_array_copy_within_value(
                    state,
                    COPY_END,
                    Value::from_f64(index as f64),
                )?;
                self.finish_typed_array_copy_within_state(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts primitive indices in specification order before touching the backing again.
    fn finish_primitive_typed_array_copy_within(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        length: usize,
        target: Value,
        start: Value,
        end: Value,
    ) -> Result<(), ExecutionError> {
        let target = typed_array_copy_relative_index(
            typed_array_copy_integer(self.convert_to_number(target)?)?,
            length,
        );
        let start = typed_array_copy_relative_index(
            typed_array_copy_integer(self.convert_to_number(start)?)?,
            length,
        );
        let end = if end.as_immediate() == Some(Immediate::Undefined) {
            length
        } else {
            typed_array_copy_relative_index(
                typed_array_copy_integer(self.convert_to_number(end)?)?,
                length,
            )
        };
        self.copy_typed_array_range(receiver, length, target, start, end)?;
        self.write(site.caller_base, site.destination, receiver)
    }

    /// Finishes a rooted observable path after all index conversions have completed.
    fn finish_typed_array_copy_within_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let receiver = pending.values[COPY_RECEIVER];
        let length = typed_array_copy_usize(pending.values[COPY_LENGTH])?;
        let target = typed_array_copy_usize(pending.values[COPY_TARGET])?;
        let start = typed_array_copy_usize(pending.values[COPY_START])?;
        let end = typed_array_copy_usize(pending.values[COPY_END])?;
        self.copy_typed_array_range(receiver, length, target, start, end)?;
        self.write(site.caller_base, site.destination, receiver)
    }

    /// Revalidates the current backing and performs one overlap-safe byte copy.
    fn copy_typed_array_range(
        &mut self,
        receiver: Value,
        initial_length: usize,
        target: usize,
        start: usize,
        end: usize,
    ) -> Result<(), ExecutionError> {
        let initial_count = end
            .saturating_sub(start)
            .min(initial_length.saturating_sub(target));
        if initial_count == 0 {
            return Ok(());
        }
        let snapshot = self.typed_array_snapshot(receiver)?;
        let data = self.typed_array_backing(snapshot.buffer)?;
        let count = initial_count
            .min(snapshot.length.saturating_sub(start))
            .min(snapshot.length.saturating_sub(target));
        if count == 0 {
            return Ok(());
        }
        let width = snapshot.kind.byte_width();
        let from = snapshot
            .byte_offset
            .checked_add(
                start
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let to = snapshot
            .byte_offset
            .checked_add(
                target
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let count_bytes = count
            .checked_mul(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let from_end = from
            .checked_add(count_bytes)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let to_end = to
            .checked_add(count_bytes)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        self.with_buffer_backing_bytes_mut(&data, |bytes, visible| {
            if from_end > visible
                || to_end > visible
                || from_end > bytes.len()
                || to_end > bytes.len()
            {
                return Err(ExecutionError::InvalidArrayLength);
            }
            bytes.copy_within(from..from_end, to);
            Ok(())
        })
    }

    /// Dispatches one object conversion while the complete fixed state remains traced.
    fn convert_typed_array_copy_within_index(
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
        self.resume_typed_array_copy_within_conversion(site, state, consumer, value)
    }

    /// Updates one traced state slot and publishes its generational edge.
    fn set_typed_array_copy_within_value(
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

    /// Allocates observable copy state under the complete VM root set.
    fn allocate_typed_array_copy_within_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArrayCopyWithinRoots {
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
}

#[inline(always)]
fn typed_array_copy_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    })
}

#[inline(always)]
fn typed_array_copy_relative_index(relative: f64, length: usize) -> usize {
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
fn typed_array_copy_usize(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}
