//! TypedArray entry point for the shared resumable stable merge machine.

use super::*;

impl Isolate {
    /// Snapshots a fixed TypedArray before starting observable comparator calls.
    pub(crate) fn begin_typed_array_callable_sort(
        &mut self,
        site: &CallSite,
        comparator: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let initial = self.validated_typed_array_snapshot(receiver)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let capacity =
            u64::try_from(initial.length).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let state = self.allocate_array_to_sorted_state(PendingArrayToSorted {
            receiver,
            result: receiver,
            comparator,
            left_value: undefined,
            right_value: undefined,
            left_string: undefined,
            retained: undefined,
            values: exact_value_buffer(capacity, undefined)?,
            scratch: exact_value_buffer(capacity, undefined)?,
            length: capacity,
            item_count: capacity,
            cursor: 0,
            width: 1,
            merge_start: 0,
            left: 0,
            left_end: 0,
            right: 0,
            right_end: 0,
            destination: 0,
            active_merge: false,
            copy: false,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_to_sorted_state(native_site, state)?;

        // Revalidate the receiver before every potentially allocating BigInt read. The managed
        // state, not this Rust loop, owns all values once observable comparator work begins.
        for index in 0..initial.length {
            let snapshot = self.typed_array_snapshot(receiver)?;
            let value = self.typed_array_read_element(snapshot, index)?;
            let state = self.pending_array_to_sorted_reference(
                self.read(native_site.caller_base, native_site.destination)?,
            )?;
            self.set_array_to_sorted_buffer_value(state, false, index as u64, value)?;
        }
        let state = self.pending_array_to_sorted_reference(
            self.read(native_site.caller_base, native_site.destination)?,
        )?;
        self.advance_array_to_sorted_merge(native_site, state)
    }
}
