//! Array.prototype.map entry point for the shared resumable iteration machine.

use super::*;
use crate::tuning::arrays::ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD;

impl Isolate {
    /// Captures map inputs before observable length/species work begins.
    pub(crate) fn begin_array_map(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let undefined = Value::from_immediate(Immediate::Undefined);
        let output = self.allocate_array_for_each_state(NativeCallState {
            values: [
                undefined,
                this_argument,
                Value::from_i32(0),
                undefined,
                undefined,
            ],
            count: MAP_STATE_COUNT,
        })?;
        let state = self.allocate_array_for_each_state(NativeCallState {
            values: [
                receiver,
                callback,
                Value::from_heap_ref(output.raw()),
                Value::from_i32(0),
                Value::from_i32(0),
            ],
            count: 5,
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

    /// Fast-forwards shared forward iterators across long ordinary hole runs.
    pub(super) fn skip_array_iteration_holes(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = exact_nonnegative_integer(pending.values[FOREACH_LENGTH])?;
        let cursor = exact_nonnegative_integer(pending.values[FOREACH_NEXT_INDEX])?;
        if length.saturating_sub(cursor) <= ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD {
            return Ok(());
        }
        let Some(next) = self.next_forward_array_candidate(
            pending.values[FOREACH_RECEIVER],
            length,
            cursor - 1,
        )?
        else {
            return Ok(());
        };
        self.set_array_for_each_number(state, FOREACH_NEXT_INDEX, next)
    }

    /// Finds the next possible numeric property without invoking getters or Proxy traps.
    fn next_forward_array_candidate(
        &mut self,
        receiver: Value,
        length: u64,
        current_index: u64,
    ) -> Result<Option<u64>, ExecutionError> {
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
                    .filter(|index| current_index < *index && *index < length)
                else {
                    continue;
                };
                candidate = Some(candidate.map_or(index, |old: u64| old.min(index)));
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                break;
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
        Ok(Some(candidate.unwrap_or(length)))
    }
}
