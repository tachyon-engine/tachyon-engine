//! Resumable fixed Number `%TypedArray.prototype.join%`.

use super::*;

const JOIN_RECEIVER: usize = 0;
const JOIN_LENGTH: usize = 1;
const JOIN_STATE_SLOTS: u8 = 2;

struct TypedArrayJoinRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArrayJoinRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the fixed view before separator conversion and preserves its internal length.
    pub(crate) fn begin_typed_array_join(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let snapshot = self.typed_array_snapshot(receiver)?;
        self.typed_array_backing(snapshot.buffer)?;
        let separator = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(separator) {
            let state = self.allocate_typed_array_join_state(NativeCallState {
                values: [
                    receiver,
                    Value::from_f64(snapshot.length as f64),
                    Value::from_immediate(Immediate::Undefined),
                    Value::from_immediate(Immediate::Undefined),
                    Value::from_immediate(Immediate::Undefined),
                ],
                count: JOIN_STATE_SLOTS,
            })?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::TypedArrayJoinSeparator,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                separator,
                site.call_site,
            );
        }
        self.finish_typed_array_join(continuation_site, receiver, snapshot.length, separator)
    }

    /// Resumes separator ToString and enters the allocation-free numeric scan.
    pub(crate) fn resume_typed_array_join_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        separator: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        self.finish_typed_array_join(
            site,
            pending.values[JOIN_RECEIVER],
            typed_array_join_length(pending.values[JOIN_LENGTH])?,
            separator,
        )
    }

    /// Builds an exact-capacity UTF-16 result in two explicit fixed-view scans.
    fn finish_typed_array_join(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        length: usize,
        separator: Value,
    ) -> Result<(), ExecutionError> {
        let separator = self.typed_array_join_separator(separator)?;
        let snapshot = self.typed_array_snapshot(receiver)?;
        let attached = match self.typed_array_backing(snapshot.buffer) {
            Ok(_) => true,
            Err(ExecutionError::DetachedArrayBuffer) => false,
            Err(error) => return Err(error),
        };
        // A separator conversion may detach the backing. Integer-indexed Get then
        // yields `undefined` for each element, but the length captured before
        // conversion still determines the number of separators in the result.
        let effective_length = if attached {
            length.min(snapshot.length)
        } else {
            length
        };
        let mut output_length = separator
            .len()
            .checked_mul(effective_length.saturating_sub(1))
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        if attached {
            for index in 0..effective_length {
                if let Some(value) = self.typed_array_join_element(snapshot, index)? {
                    output_length = output_length
                        .checked_add(self.primitive_string_unit_length(value)?)
                        .ok_or(ExecutionError::StringBufferAllocationFailed)?;
                }
            }
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(output_length)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..effective_length {
            if index != 0 {
                output.extend_from_slice(&separator);
            }
            if attached && let Some(value) = self.typed_array_join_element(snapshot, index)? {
                self.append_primitive_string_units(value, &mut output)?;
            }
        }
        debug_assert_eq!(output.len(), output_length);
        let string = JsString::try_from_owned_code_units(output)
            .map_err(ExecutionError::PropertyKeyString)?;
        let value = self.allocate_runtime_string(string)?;
        self.write(site.caller_base, site.destination, value)
    }

    /// Maps post-separator detach to the undefined element required by integer-indexed Get.
    fn typed_array_join_element(
        &mut self,
        snapshot: TypedArraySnapshot,
        index: usize,
    ) -> Result<Option<Value>, ExecutionError> {
        match self.typed_array_read_element(snapshot, index) {
            Ok(value) => Ok(Some(value)),
            Err(ExecutionError::DetachedArrayBuffer) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Converts a primitive separator with the shared ECMAScript formatter and exact sizing.
    fn typed_array_join_separator(&mut self, separator: Value) -> Result<Vec<u16>, ExecutionError> {
        if separator.as_immediate() == Some(Immediate::Undefined) {
            return Ok(vec![u16::from(b',')]);
        }
        let capacity = self.primitive_string_unit_length(separator)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        self.append_primitive_string_units(separator, &mut units)?;
        debug_assert_eq!(units.len(), capacity);
        Ok(units)
    }

    /// Allocates the fixed two-slot state under all VM roots before separator ToString.
    fn allocate_typed_array_join_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArrayJoinRoots {
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
fn typed_array_join_length(value: Value) -> Result<usize, ExecutionError> {
    let length = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    usize::try_from(length as u64).map_err(|_| ExecutionError::InvalidArrayLength)
}
