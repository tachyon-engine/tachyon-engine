//! Resumable fixed-buffer `%TypedArray.prototype%.includes` search.

use super::*;

const INCLUDES_RECEIVER: usize = 0;
const INCLUDES_SEARCH: usize = 1;
const INCLUDES_FROM_INDEX: usize = 2;
const INCLUDES_LENGTH: usize = 3;
const INCLUDES_STATE_SLOTS: u8 = 4;

struct TypedArrayIncludesRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArrayIncludesRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the receiver, preserves the initial length, and converts only fromIndex.
    pub(crate) fn begin_typed_array_includes(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let snapshot = self.typed_array_includes_snapshot(receiver)?;
        if snapshot.length == 0 {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::False),
            );
        }
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let from_index = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(from_index) {
            return self.begin_typed_array_includes_object_index(
                continuation_site,
                receiver,
                search,
                from_index,
                snapshot.length,
            );
        }
        self.finish_typed_array_includes(
            continuation_site,
            receiver,
            search,
            snapshot.length,
            from_index,
        )
    }

    /// Restores the rooted receiver/search pair after observable object-to-primitive conversion.
    pub(crate) fn resume_typed_array_includes_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = numeric_value(pending.values[INCLUDES_LENGTH])
            .ok_or(ExecutionError::InvalidArrayLength)? as usize;
        self.finish_typed_array_includes(
            site,
            pending.values[INCLUDES_RECEIVER],
            pending.values[INCLUDES_SEARCH],
            length,
            value,
        )
    }

    /// Roots all observable inputs before allocating conversion machinery.
    fn begin_typed_array_includes_object_index(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        search: Value,
        from_index: Value,
        length: usize,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_typed_array_includes_state(NativeCallState {
            values: [
                receiver,
                search,
                from_index,
                Value::from_f64(length as f64),
                undefined,
            ],
            count: INCLUDES_STATE_SLOTS,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let pending = self.native_call_state_snapshot(state)?;
        self.dispatch_object_primitive_conversion(
            ConversionConsumer::TypedArrayIncludesFromIndex,
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
            pending.values[INCLUDES_FROM_INDEX],
            site.call_site,
        )
    }

    /// Normalizes ToIntegerOrInfinity and scans the latest fixed backing up to the initial length.
    fn finish_typed_array_includes(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        search: Value,
        initial_length: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let integer = if number.is_nan() || number == 0.0 {
            0.0
        } else {
            number.trunc()
        };
        let cursor = typed_array_includes_cursor(initial_length, integer);
        let found = if cursor >= initial_length {
            false
        } else {
            let snapshot = self.typed_array_snapshot(receiver)?;
            if search.as_immediate() == Some(Immediate::Undefined)
                && matches!(
                    self.typed_array_backing(snapshot.buffer),
                    Err(ExecutionError::DetachedArrayBuffer)
                )
            {
                true
            } else {
                match self.scan_typed_array_includes(snapshot, initial_length, cursor, search) {
                    Ok(found) => found,
                    Err(ExecutionError::DetachedArrayBuffer) => {
                        search.as_immediate() == Some(Immediate::Undefined)
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        self.write(site.caller_base, site.destination, boolean_value(found))
    }

    /// Scans normalized Number or BigInt bits under one checked no-GC backing borrow.
    fn scan_typed_array_includes(
        &mut self,
        snapshot: TypedArraySnapshot,
        initial_length: usize,
        cursor: usize,
        search: Value,
    ) -> Result<bool, ExecutionError> {
        let Some(search) = self.typed_array_search_needle(snapshot.kind, search)? else {
            return Ok(false);
        };
        let length = initial_length.min(snapshot.length);
        let width = snapshot.kind.byte_width();
        let end = snapshot
            .byte_offset
            .checked_add(
                length
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let data = self.typed_array_backing(snapshot.buffer)?;
        self.with_buffer_backing_bytes(&data, |data, visible| {
            if end > visible || end > data.len() {
                return Err(ExecutionError::InvalidArrayLength);
            }
            for index in cursor..length {
                let start = snapshot.byte_offset + index * width;
                let mut bytes = [0_u8; 8];
                bytes[..width].copy_from_slice(&data[start..start + width]);
                let equal = match search {
                    TypedArraySearchNeedle::Number(search) => {
                        let element = numeric_value(data_view_decode(
                            data_view_kind(snapshot.kind)?,
                            bytes,
                            true,
                        ))
                        .expect("Number TypedArray decoding always returns Number");
                        (search.is_nan() && element.is_nan()) || search == element
                    }
                    TypedArraySearchNeedle::BigInt(search) => search == u64::from_le_bytes(bytes),
                };
                if equal {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    /// Validates the fixed view and proves its current ArrayBuffer backing is attached.
    fn typed_array_includes_snapshot(
        &mut self,
        receiver: Value,
    ) -> Result<TypedArraySnapshot, ExecutionError> {
        let snapshot = self.validated_typed_array_snapshot(receiver)?;
        Ok(snapshot)
    }

    /// Allocates the bounded conversion state under the complete VM root set.
    fn allocate_typed_array_includes_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArrayIncludesRoots {
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

/// Converts a relative integer index into the bounded forward scan cursor.
#[inline(always)]
fn typed_array_includes_cursor(length: usize, integer: f64) -> usize {
    if integer == f64::INFINITY {
        return length;
    }
    if integer >= 0.0 {
        return (integer as usize).min(length);
    }
    if integer == f64::NEG_INFINITY {
        return 0;
    }
    (length as f64 + integer).max(0.0) as usize
}
