//! Resumable Array.prototype.reduce and reduceRight accumulation state.

use super::*;
use crate::tuning::arrays::ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD;

const REDUCE_RECEIVER: usize = 0;
const REDUCE_CALLBACK: usize = 1;
const REDUCE_ACCUMULATOR: usize = 2;
const REDUCE_LENGTH: usize = 3;
const REDUCE_CURSOR: usize = 4;
const REDUCE_FORWARD_UNINITIALIZED: u8 = 10;
const REDUCE_FORWARD_INITIALIZED: u8 = 11;
const REDUCE_REVERSE_UNINITIALIZED: u8 = 12;
const REDUCE_REVERSE_INITIALIZED: u8 = 13;

impl Isolate {
    /// Starts reduce or reduceRight after publishing the fixed receiver/callback/accumulator state.
    pub(crate) fn begin_array_reduce(
        &mut self,
        site: &CallSite,
        reverse: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let initial = self.call_argument(site, 1)?;
        let initialized = initial.is_some();
        let mode = match (reverse, initialized) {
            (false, false) => REDUCE_FORWARD_UNINITIALIZED,
            (false, true) => REDUCE_FORWARD_INITIALIZED,
            (true, false) => REDUCE_REVERSE_UNINITIALIZED,
            (true, true) => REDUCE_REVERSE_INITIALIZED,
        };
        let state = self.allocate_array_for_each_state(NativeCallState {
            values: [
                receiver,
                callback,
                initial.unwrap_or(Value::from_immediate(Immediate::Undefined)),
                Value::from_i32(0),
                Value::from_i32(0),
            ],
            count: mode,
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

    /// Identifies the fixed state shape used only by the two reduction builtins.
    pub(super) fn is_array_reduce_state(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<bool, ExecutionError> {
        let count = self.native_call_state_snapshot(state)?.count;
        Ok(matches!(
            count,
            REDUCE_FORWARD_UNINITIALIZED
                | REDUCE_FORWARD_INITIALIZED
                | REDUCE_REVERSE_UNINITIALIZED
                | REDUCE_REVERSE_INITIALIZED
        ))
    }

    /// Advances the logical reduction cursor without representing reverse indexes as signed values.
    pub(super) fn advance_array_reduce(
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
            let length = exact_nonnegative_integer(pending.values[REDUCE_LENGTH])?;
            let cursor = exact_nonnegative_integer(pending.values[REDUCE_CURSOR])?;
            if cursor >= length {
                if is_reduce_initialized(pending.count) {
                    return self.write(
                        site.caller_base,
                        site.destination,
                        pending.values[REDUCE_ACCUMULATOR],
                    );
                }
                return Err(ExecutionError::ArrayReduceEmpty);
            }
            self.set_array_for_each_number(state, REDUCE_CURSOR, cursor + 1)?;
            let index = reduce_index(pending.count, length, cursor);
            let key = Value::from_f64(index as f64);
            let Some(has) = self.dispatch_array_iteration_has(
                site,
                state,
                ArrayForEachStage::ReduceHas,
                pending.values[REDUCE_RECEIVER],
                key,
            )?
            else {
                return Ok(());
            };
            if !self.is_truthy_value(has)? {
                self.skip_array_reduce_holes(state)?;
                continue;
            }
            let Some(value) = self.dispatch_array_reduce_get(site, state)? else {
                return Ok(());
            };
            if self.consume_array_reduce_element(site, state, value)? {
                return Ok(());
            }
        }
    }

    /// Completes a resumed HasProperty result and starts the corresponding indexed Get.
    pub(super) fn resume_array_reduce_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        if !self.is_truthy_value(value)? {
            self.skip_array_reduce_holes(state)?;
            return self.advance_array_reduce(site, state);
        }
        let Some(value) = self.dispatch_array_reduce_get(site, state)? else {
            return Ok(());
        };
        self.resume_array_reduce_get(site, state, value)
    }

    /// Dispatches the element Get for the already-advanced logical cursor.
    fn dispatch_array_reduce_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = exact_nonnegative_integer(pending.values[REDUCE_LENGTH])?;
        let cursor = exact_nonnegative_integer(pending.values[REDUCE_CURSOR])?;
        let index = reduce_index(pending.count, length, cursor - 1);
        let key = self.property_key_atom(Value::from_f64(index as f64))?;
        self.dispatch_array_for_each_get(
            site,
            state,
            ArrayForEachStage::ReduceGet,
            pending.values[REDUCE_RECEIVER],
            key.into(),
        )
    }

    /// Continues the explicit loop after a resumed indexed Get unless the callback suspends.
    pub(super) fn resume_array_reduce_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.consume_array_reduce_element(site, state, value)? {
            Ok(())
        } else {
            self.advance_array_reduce(site, state)
        }
    }

    /// Selects the first element as accumulator or enters the four-argument callback boundary.
    fn consume_array_reduce_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<bool, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        if !is_reduce_initialized(pending.count) {
            self.set_array_for_each_value(state, REDUCE_ACCUMULATOR, value)?;
            self.set_reduce_initialized(state)?;
            return Ok(false);
        }
        let length = exact_nonnegative_integer(pending.values[REDUCE_LENGTH])?;
        let cursor = exact_nonnegative_integer(pending.values[REDUCE_CURSOR])?;
        let index = Value::from_f64(reduce_index(pending.count, length, cursor - 1) as f64);
        let Some(returned) = self.call_array_reduce_callback(site, state, pending, value, index)?
        else {
            return Ok(true);
        };
        self.set_array_for_each_value(state, REDUCE_ACCUMULATOR, returned)?;
        Ok(false)
    }

    /// Calls one reducer callback while the fixed state remains in the native completion roots.
    fn call_array_reduce_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        pending: NativeCallState,
        value: Value,
        index: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        let continuation = NativeContinuation::array_for_each(
            site,
            ArrayForEachStage::ReduceCallback,
            Value::from_heap_ref(state.raw()),
            value,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Isolate::completion_stack_error)?;
        let prefix = match self.create_apply_argument_prefix(
            pending.values[REDUCE_CALLBACK],
            Value::from_immediate(Immediate::Undefined),
            vec![
                pending.values[REDUCE_ACCUMULATOR],
                value,
                index,
                pending.values[REDUCE_RECEIVER],
            ],
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
            callee: pending.values[REDUCE_CALLBACK],
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 4,
            argument_count: 4,
            this_value: Value::from_immediate(Immediate::Undefined),
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
                .expect("Array reduce callback publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        Ok(Some(returned))
    }

    /// Stores a callback result as accumulator and resumes the logical cursor.
    pub(super) fn finish_array_reduce_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_for_each_value(state, REDUCE_ACCUMULATOR, value)?;
        self.advance_array_reduce(site, state)
    }

    /// Marks a reduction state initialized without consuming another Value slot.
    fn set_reduce_initialized(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.count = if state.count == REDUCE_REVERSE_UNINITIALIZED {
                    REDUCE_REVERSE_INITIALIZED
                } else {
                    REDUCE_FORWARD_INITIALIZED
                };
                Ok::<(), ExecutionError>(())
            })
        })
    }

    /// Fast-forwards a long ordinary hole run while preserving Proxy HasProperty observability.
    fn skip_array_reduce_holes(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = exact_nonnegative_integer(pending.values[REDUCE_LENGTH])?;
        let cursor = exact_nonnegative_integer(pending.values[REDUCE_CURSOR])?;
        if length.saturating_sub(cursor) <= ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD {
            return Ok(());
        }
        let Some(next_cursor) = self.next_array_reduce_candidate(
            pending.values[REDUCE_RECEIVER],
            pending.count,
            length,
            cursor,
        )?
        else {
            return Ok(());
        };
        self.set_array_for_each_number(state, REDUCE_CURSOR, next_cursor)
    }

    /// Finds the next possible numeric property without invoking getters or Proxy traps.
    fn next_array_reduce_candidate(
        &mut self,
        receiver: Value,
        mode: u8,
        length: u64,
        cursor: u64,
    ) -> Result<Option<u64>, ExecutionError> {
        let reverse = is_reduce_reverse(mode);
        let current_index = reduce_index(mode, length, cursor - 1);
        let mut candidate = None;
        let mut current = receiver;
        loop {
            if self.is_proxy_value(current) {
                return Ok(None);
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            let mut keys = self.ordinary_own_property_keys(current, snapshot)?;
            while let Some(entry) = keys.next_entry() {
                let Some(atom) = entry.key.atom() else {
                    continue;
                };
                let Some(index) = self
                    .atoms
                    .get(atom)
                    .and_then(|string| safe_integer_index(string.as_view()))
                    .filter(|index| *index < length)
                else {
                    continue;
                };
                if reverse && index < current_index {
                    candidate = Some(candidate.map_or(index, |old: u64| old.max(index)));
                } else if !reverse && index > current_index {
                    candidate = Some(candidate.map_or(index, |old: u64| old.min(index)));
                }
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                break;
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
        Ok(Some(candidate.map_or(length, |index| {
            if reverse { length - index - 1 } else { index }
        })))
    }
}

#[inline(always)]
fn is_reduce_initialized(mode: u8) -> bool {
    mode == REDUCE_FORWARD_INITIALIZED || mode == REDUCE_REVERSE_INITIALIZED
}

#[inline(always)]
fn is_reduce_reverse(mode: u8) -> bool {
    mode == REDUCE_REVERSE_UNINITIALIZED || mode == REDUCE_REVERSE_INITIALIZED
}

#[inline(always)]
fn reduce_index(mode: u8, length: u64, cursor: u64) -> u64 {
    if is_reduce_reverse(mode) {
        length - cursor - 1
    } else {
        cursor
    }
}

/// Parses canonical decimal property names in the complete safe-integer index range.
pub(super) fn safe_integer_index(string: JsStringView<'_>) -> Option<u64> {
    let length = string.len();
    if length == 0 || length > 16 {
        return None;
    }
    let first = string.code_unit_at(0)?;
    if first == u16::from(b'0') {
        return (length == 1).then_some(0);
    }
    if !(u16::from(b'1')..=u16::from(b'9')).contains(&first) {
        return None;
    }
    let mut value = u64::from(first - u16::from(b'0'));
    for index in 1..length {
        let unit = string.code_unit_at(index)?;
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&unit) {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(unit - u16::from(b'0')))?;
    }
    (value <= MAX_SAFE_INTEGER).then_some(value)
}

#[inline(always)]
fn exact_nonnegative_integer(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return Err(ExecutionError::UnsupportedNumberConversion(value));
    }
    Ok(number as u64)
}
