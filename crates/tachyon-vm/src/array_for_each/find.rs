//! Resumable Array find-family iteration, including hole visits.

use super::*;

const FIND_FORWARD_VALUE: u8 = 30;
const FIND_FORWARD_INDEX: u8 = 31;
const FIND_REVERSE_VALUE: u8 = 32;
const FIND_REVERSE_INDEX: u8 = 33;

impl Isolate {
    /// Captures find inputs before observable length lookup and callback validation.
    pub(crate) fn begin_array_find(
        &mut self,
        site: &CallSite,
        reverse: bool,
        return_index: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let mode = match (reverse, return_index) {
            (false, false) => FIND_FORWARD_VALUE,
            (false, true) => FIND_FORWARD_INDEX,
            (true, false) => FIND_REVERSE_VALUE,
            (true, true) => FIND_REVERSE_INDEX,
        };
        let state = self.allocate_array_for_each_state(NativeCallState {
            values: [
                receiver,
                callback,
                this_argument,
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

    /// Identifies the four scalar modes owned by the Array find family.
    pub(super) fn is_array_find_state(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<bool, ExecutionError> {
        Ok(matches!(
            self.native_call_state_snapshot(state)?.count,
            FIND_FORWARD_VALUE | FIND_FORWARD_INDEX | FIND_REVERSE_VALUE | FIND_REVERSE_INDEX
        ))
    }

    /// Initializes the direction-specific cursor after length and callable validation.
    pub(super) fn begin_array_find_after_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = exact_nonnegative_integer(pending.values[FOREACH_LENGTH])?;
        if find_reverse(pending.count) {
            self.set_array_for_each_number(state, FOREACH_NEXT_INDEX, length)?;
        }
        self.advance_array_find(site, state)
    }

    /// Advances direct Get and callback steps until an observable operation suspends.
    fn advance_array_find(
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
            let length = exact_nonnegative_integer(pending.values[FOREACH_LENGTH])?;
            let cursor = exact_nonnegative_integer(pending.values[FOREACH_NEXT_INDEX])?;
            let reverse = find_reverse(pending.count);
            if (!reverse && cursor >= length) || (reverse && cursor == 0) {
                return self.write(
                    site.caller_base,
                    site.destination,
                    find_miss_value(pending.count),
                );
            }
            let index = if reverse { cursor - 1 } else { cursor };
            self.set_array_for_each_number(
                state,
                FOREACH_NEXT_INDEX,
                if reverse { index } else { index + 1 },
            )?;
            let key = self.safe_integer_property_atom(index)?;
            let Some(element) = self.dispatch_array_for_each_get(
                site,
                state,
                ArrayForEachStage::FindGet,
                pending.values[FOREACH_RECEIVER],
                key.into(),
            )?
            else {
                return Ok(());
            };
            let Some(returned) = self.call_array_find_callback(site, state, element, index)? else {
                return Ok(());
            };
            if self.finish_array_find_callback(site, state, returned, element, index)? {
                return Ok(());
            }
        }
    }

    /// Resumes an indexed Get and dispatches the predicate for holes as undefined too.
    pub(super) fn resume_array_find_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        element: Value,
    ) -> Result<(), ExecutionError> {
        let index = self.array_find_current_index(state)?;
        let Some(returned) = self.call_array_find_callback(site, state, element, index)? else {
            return Ok(());
        };
        if self.finish_array_find_callback(site, state, returned, element, index)? {
            Ok(())
        } else {
            self.advance_array_find(site, state)
        }
    }

    /// Resumes predicate completion using the element retained by the typed continuation.
    pub(super) fn resume_array_find_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        returned: Value,
        element: Value,
    ) -> Result<(), ExecutionError> {
        let index = self.array_find_current_index(state)?;
        if self.finish_array_find_callback(site, state, returned, element, index)? {
            Ok(())
        } else {
            self.advance_array_find(site, state)
        }
    }

    /// Calls the predicate with `(element, index, boxed receiver)` and the captured thisArg.
    fn call_array_find_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        element: Value,
        index: u64,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        self.call_array_iteration_callback(
            site,
            state,
            element,
            safe_integer_value(index),
            pending.values[FOREACH_THIS_ARGUMENT],
            ArrayForEachStage::FindCallback,
        )
    }

    /// Selects the value/index result after applying predicate ToBoolean.
    fn finish_array_find_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        returned: Value,
        element: Value,
        index: u64,
    ) -> Result<bool, ExecutionError> {
        if !self.is_truthy_value(returned)? {
            return Ok(false);
        }
        let mode = self.native_call_state_snapshot(state)?.count;
        let result = if find_returns_index(mode) {
            safe_integer_value(index)
        } else {
            element
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(true)
    }

    /// Recovers the index whose cursor was advanced before the observable Get.
    fn array_find_current_index(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<u64, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let cursor = exact_nonnegative_integer(pending.values[FOREACH_NEXT_INDEX])?;
        Ok(if find_reverse(pending.count) {
            cursor
        } else {
            cursor - 1
        })
    }
}

#[inline(always)]
fn find_reverse(mode: u8) -> bool {
    matches!(mode, FIND_REVERSE_VALUE | FIND_REVERSE_INDEX)
}

#[inline(always)]
fn find_returns_index(mode: u8) -> bool {
    matches!(mode, FIND_FORWARD_INDEX | FIND_REVERSE_INDEX)
}

#[inline(always)]
fn find_miss_value(mode: u8) -> Value {
    if find_returns_index(mode) {
        Value::from_i32(-1)
    } else {
        Value::from_immediate(Immediate::Undefined)
    }
}
