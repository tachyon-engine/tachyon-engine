//! Resumable `%TypedArray.prototype.with%` change-by-copy implementation.

use super::*;

const WITH_RECEIVER: usize = 0;
const WITH_LENGTH: usize = 1;
const WITH_INDEX: usize = 2;
const WITH_VALUE: usize = 3;
const WITH_STATE_SLOTS: u8 = 5;

struct TypedArrayWithRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArrayWithRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the initial fixed view and starts index conversion before value conversion.
    pub(crate) fn begin_typed_array_with(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let snapshot = self.typed_array_snapshot(receiver)?;
        self.typed_array_backing(snapshot.buffer)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let index = self.call_argument(site, 0)?.unwrap_or(undefined);
        let value = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_typed_array_with_state(NativeCallState {
            values: [
                receiver,
                Value::from_f64(snapshot.length as f64),
                index,
                value,
                undefined,
            ],
            count: WITH_STATE_SLOTS,
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
        self.convert_typed_array_with_value(
            continuation_site,
            state,
            ConversionConsumer::TypedArrayWithIndex,
            index,
        )
    }

    /// Resumes index then replacement conversion while preserving their observable order.
    pub(crate) fn resume_typed_array_with_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::TypedArrayWithIndex => {
                let index = typed_array_with_integer(self.convert_to_number(value)?)?;
                let original_length = typed_array_with_usize(
                    self.native_call_state_snapshot(state)?.values[WITH_LENGTH],
                )?;
                let actual_index = if index < 0.0 {
                    original_length as f64 + index
                } else {
                    index
                };
                self.set_typed_array_with_value(state, WITH_INDEX, Value::from_f64(actual_index))?;
                let replacement = self.native_call_state_snapshot(state)?.values[WITH_VALUE];
                self.convert_typed_array_with_value(
                    site,
                    state,
                    ConversionConsumer::TypedArrayWithValue,
                    replacement,
                )
            }
            ConversionConsumer::TypedArrayWithValue => {
                let receiver = self.native_call_state_snapshot(state)?.values[WITH_RECEIVER];
                let kind = self.typed_array_snapshot(receiver)?.kind;
                let replacement = match kind.content_type() {
                    ContentType::Number => self.convert_to_number(value)?,
                    ContentType::BigInt => self.primitive_to_bigint(value)?,
                };
                self.set_typed_array_with_value(state, WITH_VALUE, replacement)?;
                self.finish_typed_array_with(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Revalidates the current source, copies it, and replaces exactly one result element.
    fn finish_typed_array_with(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let source = pending.values[WITH_RECEIVER];
        let original_length = typed_array_with_usize(pending.values[WITH_LENGTH])?;
        let index = typed_array_with_valid_index(pending.values[WITH_INDEX])?;
        let snapshot = self.typed_array_snapshot(source)?;
        match self.typed_array_backing(snapshot.buffer) {
            Ok(_) => {}
            Err(ExecutionError::DetachedArrayBuffer) => {
                return Err(ExecutionError::InvalidArrayLength);
            }
            Err(error) => return Err(error),
        }
        if index >= snapshot.length {
            return Err(ExecutionError::InvalidArrayLength);
        }

        let target = self.create_fixed_typed_array_same_kind(snapshot.kind, original_length)?;
        self.copy_same_kind_typed_array(source, target)?;
        let target_snapshot = self.typed_array_snapshot(target)?;
        if index < target_snapshot.length {
            self.typed_array_write_value(target_snapshot, index, pending.values[WITH_VALUE])?;
        }
        self.write(site.caller_base, site.destination, target)
    }

    /// Dispatches object ToPrimitive while the full operation remains in traced state.
    fn convert_typed_array_with_value(
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
        self.resume_typed_array_with_conversion(site, state, consumer, value)
    }

    /// Publishes one state edge before any later allocation or callback boundary.
    fn set_typed_array_with_value(
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

    /// Allocates fixed-capacity operation state under the complete VM root set.
    fn allocate_typed_array_with_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArrayWithRoots {
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
fn typed_array_with_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    })
}

#[inline(always)]
fn typed_array_with_usize(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}

#[inline(always)]
fn typed_array_with_valid_index(value: Value) -> Result<usize, ExecutionError> {
    typed_array_with_usize(value)
}
