//! Resumable indexOf and lastIndexOf strict-equality search.

use super::*;
use crate::tuning::arrays::ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD;

const SEARCH_RECEIVER: usize = 0;
const SEARCH_ELEMENT: usize = 1;
const SEARCH_FROM_INDEX: usize = 2;
const SEARCH_LENGTH: usize = 3;
const SEARCH_CURSOR: usize = 4;
const SEARCH_FORWARD: u8 = 20;
const SEARCH_REVERSE_DEFAULT: u8 = 21;
const SEARCH_REVERSE_EXPLICIT: u8 = 22;

impl Isolate {
    /// Captures search inputs before the observable length and fromIndex conversions.
    pub(crate) fn begin_array_index_search(
        &mut self,
        site: &CallSite,
        reverse: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let explicit_from = site.argument_count > 1;
        let from_index = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let mode = match (reverse, explicit_from) {
            (false, _) => SEARCH_FORWARD,
            (true, false) => SEARCH_REVERSE_DEFAULT,
            (true, true) => SEARCH_REVERSE_EXPLICIT,
        };
        let state = self.allocate_array_for_each_state(NativeCallState {
            values: [
                receiver,
                search,
                from_index,
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

    /// Identifies the fixed state modes owned by the two index search methods.
    pub(super) fn is_array_search_state(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<bool, ExecutionError> {
        Ok(matches!(
            self.native_call_state_snapshot(state)?.count,
            SEARCH_FORWARD | SEARCH_REVERSE_DEFAULT | SEARCH_REVERSE_EXPLICIT
        ))
    }

    /// Skips fromIndex conversion for empty receivers, otherwise starts ToIntegerOrInfinity.
    pub(super) fn begin_array_search_index(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let pending = self.native_call_state_snapshot(state)?;
        let length = exact_nonnegative_integer(pending.values[SEARCH_LENGTH])?;
        if length == 0 {
            return self.write(site.caller_base, site.destination, Value::from_i32(-1));
        }
        if pending.count == SEARCH_REVERSE_DEFAULT {
            self.set_array_for_each_number(state, SEARCH_CURSOR, length)?;
            return self.advance_array_search(site, state);
        }
        let from_index = pending.values[SEARCH_FROM_INDEX];
        if self.is_object_value(from_index) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArraySearchIndex,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                from_index,
                site.call_site,
            );
        }
        self.resume_array_search_after_index_primitive(site, state, from_index)
    }

    /// Normalizes a primitive fromIndex and enters the direction-parameterized search loop.
    pub(crate) fn resume_array_search_after_index_primitive(
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
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let integer = to_integer_or_infinity(number);
        let pending = self.native_call_state_snapshot(state)?;
        let length = exact_nonnegative_integer(pending.values[SEARCH_LENGTH])?;
        let cursor = search_cursor(pending.count, length, integer);
        self.set_array_for_each_number(state, SEARCH_CURSOR, cursor)?;
        self.advance_array_search(site, state)
    }

    /// Advances synchronous Has/Get steps and exits when an observable operation suspends.
    fn advance_array_search(
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
            let length = exact_nonnegative_integer(pending.values[SEARCH_LENGTH])?;
            let cursor = exact_nonnegative_integer(pending.values[SEARCH_CURSOR])?;
            let reverse = is_search_reverse(pending.count);
            if (!reverse && cursor >= length) || (reverse && cursor == 0) {
                return self.write(site.caller_base, site.destination, Value::from_i32(-1));
            }
            let index = if reverse { cursor - 1 } else { cursor };
            self.set_array_for_each_number(
                state,
                SEARCH_CURSOR,
                if reverse { index } else { index + 1 },
            )?;
            let Some(has) = self.dispatch_array_iteration_has(
                site,
                state,
                ArrayForEachStage::SearchHas,
                pending.values[SEARCH_RECEIVER],
                safe_integer_value(index),
            )?
            else {
                return Ok(());
            };
            if !self.is_truthy_value(has)? {
                self.skip_array_search_holes(state, index, reverse)?;
                continue;
            }
            let Some(value) = self.dispatch_array_search_get(site, state, index)? else {
                return Ok(());
            };
            if self.finish_array_search_get(site, state, value, index)? {
                return Ok(());
            }
        }
    }

    /// Resumes HasProperty and dispatches Get only for a present indexed property.
    pub(super) fn resume_array_search_has(
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
        let pending = self.native_call_state_snapshot(state)?;
        let reverse = is_search_reverse(pending.count);
        let cursor = exact_nonnegative_integer(pending.values[SEARCH_CURSOR])?;
        let index = if reverse { cursor } else { cursor - 1 };
        if !self.is_truthy_value(value)? {
            self.skip_array_search_holes(state, index, reverse)?;
            return self.advance_array_search(site, state);
        }
        let Some(value) = self.dispatch_array_search_get(site, state, index)? else {
            return Ok(());
        };
        self.resume_array_search_get(site, state, value)
    }

    /// Resumes an indexed Get and either publishes the match or continues scanning.
    pub(super) fn resume_array_search_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let cursor = exact_nonnegative_integer(pending.values[SEARCH_CURSOR])?;
        let index = if is_search_reverse(pending.count) {
            cursor
        } else {
            cursor - 1
        };
        if self.finish_array_search_get(site, state, value, index)? {
            Ok(())
        } else {
            self.advance_array_search(site, state)
        }
    }

    /// Dispatches the Proxy-aware Get for a known-present index.
    fn dispatch_array_search_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        index: u64,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let key = self.safe_integer_property_atom(index)?;
        self.dispatch_array_for_each_get(
            site,
            state,
            ArrayForEachStage::SearchGet,
            pending.values[SEARCH_RECEIVER],
            key.into(),
        )
    }

    /// Compares one loaded element using strict equality and writes its index on success.
    fn finish_array_search_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
        index: u64,
    ) -> Result<bool, ExecutionError> {
        let search = self.native_call_state_snapshot(state)?.values[SEARCH_ELEMENT];
        if !self.strict_equal_values(value, search)? {
            return Ok(false);
        }
        self.write(
            site.caller_base,
            site.destination,
            safe_integer_value(index),
        )?;
        Ok(true)
    }

    /// Fast-forwards a long ordinary hole run while preserving Proxy HasProperty observations.
    fn skip_array_search_holes(
        &mut self,
        state: GcRef<NativeCallState>,
        current_index: u64,
        reverse: bool,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = exact_nonnegative_integer(pending.values[SEARCH_LENGTH])?;
        let cursor = exact_nonnegative_integer(pending.values[SEARCH_CURSOR])?;
        let remaining = if reverse { cursor } else { length - cursor };
        if remaining <= ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD {
            return Ok(());
        }
        let Some(candidate) = self.next_array_search_candidate(
            pending.values[SEARCH_RECEIVER],
            length,
            current_index,
            reverse,
        )?
        else {
            return Ok(());
        };
        let cursor = candidate.map_or(if reverse { 0 } else { length }, |index| {
            if reverse { index + 1 } else { index }
        });
        self.set_array_for_each_number(state, SEARCH_CURSOR, cursor)
    }

    /// Finds the next possible indexed property without invoking getters or Proxy traps.
    fn next_array_search_candidate(
        &mut self,
        receiver: Value,
        length: u64,
        current_index: u64,
        reverse: bool,
    ) -> Result<Option<Option<u64>>, ExecutionError> {
        let mut candidate = None;
        let mut current = receiver;
        loop {
            if self.is_proxy_value(current) {
                return Ok(None);
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            let mut keys = self.ordinary_own_property_keys(current, snapshot)?;
            while let Some(entry) = keys.next_entry() {
                let Some(index) = entry
                    .key
                    .atom()
                    .and_then(|atom| self.atoms.get(atom))
                    .and_then(|string| super::reduce::safe_integer_index(string.as_view()))
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
        Ok(Some(candidate))
    }
}

#[inline(always)]
fn is_search_reverse(mode: u8) -> bool {
    mode != SEARCH_FORWARD
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

#[inline(always)]
fn search_cursor(mode: u8, length: u64, from_index: f64) -> u64 {
    if mode == SEARCH_FORWARD {
        if from_index >= length as f64 {
            length
        } else if from_index >= 0.0 {
            from_index as u64
        } else {
            (length as f64 + from_index).max(0.0) as u64
        }
    } else if from_index >= 0.0 {
        from_index.min((length - 1) as f64) as u64 + 1
    } else {
        let index = length as f64 + from_index;
        if index < 0.0 { 0 } else { index as u64 + 1 }
    }
}
