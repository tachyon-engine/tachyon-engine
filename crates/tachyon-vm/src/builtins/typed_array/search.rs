//! Resumable fixed TypedArray `indexOf` and `lastIndexOf` search.

use super::*;

const SEARCH_RECEIVER: usize = 0;
const SEARCH_ELEMENT: usize = 1;
const SEARCH_FROM_INDEX: usize = 2;
const SEARCH_LENGTH: usize = 3;
const SEARCH_FORWARD: u8 = 24;
const SEARCH_REVERSE: u8 = 25;

struct TypedArraySearchRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArraySearchRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the initial view and converts the direction-specific fromIndex only when nonempty.
    pub(crate) fn begin_typed_array_search(
        &mut self,
        site: &CallSite,
        direction: TypedArraySearchDirection,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let snapshot = self.typed_array_search_initial_snapshot(receiver)?;
        if snapshot.length == 0 {
            return self.write(site.caller_base, site.destination, Value::from_i32(-1));
        }
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let from_index =
            if direction == TypedArraySearchDirection::Reverse && site.argument_count <= 1 {
                safe_integer_value(snapshot.length as u64 - 1)
            } else {
                self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0))
            };
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(from_index) {
            return self.begin_typed_array_search_object_index(
                continuation_site,
                receiver,
                search,
                from_index,
                snapshot.length,
                direction,
            );
        }
        self.finish_typed_array_search(
            continuation_site,
            receiver,
            search,
            snapshot.length,
            from_index,
            direction,
        )
    }

    /// Restores the rooted search inputs after observable ToPrimitive/ToNumber work.
    pub(crate) fn resume_typed_array_search_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = numeric_value(pending.values[SEARCH_LENGTH])
            .ok_or(ExecutionError::InvalidArrayLength)? as usize;
        self.finish_typed_array_search(
            site,
            pending.values[SEARCH_RECEIVER],
            pending.values[SEARCH_ELEMENT],
            length,
            value,
            search_direction(pending.count),
        )
    }

    /// Roots receiver, search value, and conversion input before allocating continuation state.
    fn begin_typed_array_search_object_index(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        search: Value,
        from_index: Value,
        length: usize,
        direction: TypedArraySearchDirection,
    ) -> Result<(), ExecutionError> {
        let state = self.allocate_typed_array_search_state(NativeCallState {
            values: [
                receiver,
                search,
                from_index,
                Value::from_f64(length as f64),
                Value::from_immediate(Immediate::Undefined),
            ],
            count: search_mode(direction),
        })?;
        let pending = self.native_call_state_snapshot(state)?;
        self.dispatch_object_primitive_conversion(
            ConversionConsumer::TypedArraySearchFromIndex,
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
            pending.values[SEARCH_FROM_INDEX],
            site.call_site,
        )
    }

    /// Applies ToIntegerOrInfinity, revalidates backing, and publishes the strict-equality result.
    fn finish_typed_array_search(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        search: Value,
        initial_length: usize,
        value: Value,
        direction: TypedArraySearchDirection,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let integer = to_integer_or_infinity(number);
        let cursor = typed_array_search_cursor(direction, initial_length, integer);
        if cursor == search_miss_cursor(direction, initial_length) {
            return self.write(site.caller_base, site.destination, Value::from_i32(-1));
        }
        let snapshot = self.typed_array_snapshot(receiver)?;
        let backing = match self.typed_array_backing(snapshot.buffer) {
            Ok(backing) => backing,
            Err(ExecutionError::DetachedArrayBuffer) => {
                return self.write(site.caller_base, site.destination, Value::from_i32(-1));
            }
            Err(error) => return Err(error),
        };
        let result = self.scan_typed_array_search(
            snapshot,
            backing,
            initial_length,
            cursor,
            search,
            direction,
        )?;
        self.write(
            site.caller_base,
            site.destination,
            result.map_or(Value::from_i32(-1), |index| {
                safe_integer_value(index as u64)
            }),
        )
    }

    /// Scans normalized Number or BigInt bits under one checked no-GC backing borrow.
    fn scan_typed_array_search(
        &mut self,
        snapshot: TypedArraySnapshot,
        backing: GcRef<ArrayBufferData>,
        initial_length: usize,
        cursor: usize,
        search: Value,
        direction: TypedArraySearchDirection,
    ) -> Result<Option<usize>, ExecutionError> {
        let Some(search) = self.typed_array_search_needle(snapshot.kind, search)? else {
            return Ok(None);
        };
        if matches!(search, TypedArraySearchNeedle::Number(number) if number.is_nan()) {
            return Ok(None);
        }
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
        self.heap.with_running_scope(|scope| {
            let backing = scope.root(backing).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(backing, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if end > data.byte_length || end > data.bytes.len() {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                let mut index = cursor.min(length);
                while search_has_next(direction, index, length) {
                    let current = search_index(direction, index);
                    let start = snapshot.byte_offset + current * width;
                    let mut bytes = [0_u8; 8];
                    bytes[..width].copy_from_slice(&data.bytes[start..start + width]);
                    let equal = match search {
                        TypedArraySearchNeedle::Number(search) => {
                            let element = numeric_value(data_view_decode(
                                data_view_kind(snapshot.kind)?,
                                bytes,
                                true,
                            ))
                            .expect("Number TypedArray decoding always returns Number");
                            search == element
                        }
                        TypedArraySearchNeedle::BigInt(search) => {
                            search == u64::from_le_bytes(bytes)
                        }
                    };
                    if equal {
                        return Ok(Some(current));
                    }
                    index = search_advance(direction, index);
                }
                Ok(None)
            })
        })
    }

    /// Proves the receiver is a currently attached fixed Number TypedArray before any conversion.
    fn typed_array_search_initial_snapshot(
        &mut self,
        receiver: Value,
    ) -> Result<TypedArraySnapshot, ExecutionError> {
        let snapshot = self.typed_array_snapshot(receiver)?;
        self.typed_array_backing(snapshot.buffer)?;
        Ok(snapshot)
    }

    /// Allocates bounded conversion state while tracing every VM and pending Value edge.
    fn allocate_typed_array_search_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArraySearchRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
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
}

#[inline(always)]
const fn search_mode(direction: TypedArraySearchDirection) -> u8 {
    match direction {
        TypedArraySearchDirection::Forward => SEARCH_FORWARD,
        TypedArraySearchDirection::Reverse => SEARCH_REVERSE,
    }
}

#[inline(always)]
const fn search_direction(mode: u8) -> TypedArraySearchDirection {
    match mode {
        SEARCH_FORWARD => TypedArraySearchDirection::Forward,
        SEARCH_REVERSE => TypedArraySearchDirection::Reverse,
        _ => unreachable!(),
    }
}

#[inline(always)]
fn to_integer_or_infinity(number: f64) -> f64 {
    if number.is_nan() || number == 0.0 {
        0.0
    } else if number.is_infinite() {
        number
    } else {
        number.trunc()
    }
}

/// Encodes forward as the next index and reverse as the current index plus one.
#[inline(always)]
fn typed_array_search_cursor(
    direction: TypedArraySearchDirection,
    length: usize,
    from_index: f64,
) -> usize {
    match direction {
        TypedArraySearchDirection::Forward if from_index >= length as f64 => length,
        TypedArraySearchDirection::Forward if from_index >= 0.0 => from_index as usize,
        TypedArraySearchDirection::Forward => (length as f64 + from_index).max(0.0) as usize,
        TypedArraySearchDirection::Reverse if from_index == f64::NEG_INFINITY => 0,
        TypedArraySearchDirection::Reverse if from_index >= 0.0 => {
            from_index.min((length - 1) as f64) as usize + 1
        }
        TypedArraySearchDirection::Reverse => {
            let index = length as f64 + from_index;
            if index < 0.0 { 0 } else { index as usize + 1 }
        }
    }
}

#[inline(always)]
const fn search_miss_cursor(direction: TypedArraySearchDirection, length: usize) -> usize {
    match direction {
        TypedArraySearchDirection::Forward => length,
        TypedArraySearchDirection::Reverse => 0,
    }
}

#[inline(always)]
const fn search_has_next(
    direction: TypedArraySearchDirection,
    cursor: usize,
    length: usize,
) -> bool {
    match direction {
        TypedArraySearchDirection::Forward => cursor < length,
        TypedArraySearchDirection::Reverse => cursor > 0,
    }
}

#[inline(always)]
const fn search_index(direction: TypedArraySearchDirection, cursor: usize) -> usize {
    match direction {
        TypedArraySearchDirection::Forward => cursor,
        TypedArraySearchDirection::Reverse => cursor - 1,
    }
}

#[inline(always)]
const fn search_advance(direction: TypedArraySearchDirection, cursor: usize) -> usize {
    match direction {
        TypedArraySearchDirection::Forward => cursor + 1,
        TypedArraySearchDirection::Reverse => cursor - 1,
    }
}
