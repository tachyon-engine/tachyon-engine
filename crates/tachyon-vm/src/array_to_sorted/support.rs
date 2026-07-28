//! Managed state access and externally-accounted backing for stable Array sorting.

use super::*;

impl Isolate {
    /// Pushes one continuation that roots sort state and operation-local data.
    pub(super) fn push_array_to_sorted_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        stage: ArrayToSortedStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_to_sorted(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots sort state in the destination register before any safepoint.
    #[inline]
    pub(super) fn root_array_to_sorted_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates exact external sort buffers under the complete VM root set.
    pub(super) fn allocate_array_to_sorted_state(
        &mut self,
        pending: PendingArrayToSorted,
    ) -> Result<GcRef<PendingArrayToSorted>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_array_to_sorted,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Validates and recovers one managed stable-sort reference.
    pub(crate) fn pending_array_to_sorted_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayToSorted>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_to_sorted)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies sort fields without retaining a managed borrow across safepoints.
    pub(super) fn array_to_sorted_snapshot(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<ArrayToSortedSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayToSortedSnapshot {
                    receiver: pending.receiver,
                    result: pending.result,
                    comparator: pending.comparator,
                    left_value: pending.left_value,
                    right_value: pending.right_value,
                    left_string: pending.left_string,
                    retained: pending.retained,
                    length: pending.length,
                    item_count: pending.item_count,
                    cursor: pending.cursor,
                    width: pending.width,
                    merge_start: pending.merge_start,
                    left: pending.left,
                    left_end: pending.left_end,
                    right: pending.right,
                    right_end: pending.right_end,
                    destination: pending.destination,
                    active_merge: pending.active_merge,
                    copy: pending.copy,
                })
            })
        })
    }

    /// Replaces a full sparse-sort state with larger externally-accounted backing.
    pub(super) fn grow_array_sort_buffers(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        snapshot: ArrayToSortedSnapshot,
    ) -> Result<GcRef<PendingArrayToSorted>, ExecutionError> {
        let current = self.array_to_sorted_buffer_len(state)?;
        let grown = tuning::arrays::grown_array_sort_capacity(current.max(1))
            .ok_or(ExecutionError::BoundArgumentAllocationFailed)?;
        let capacity = (grown as u64).min(snapshot.length);
        if capacity <= current as u64 {
            return Err(ExecutionError::BoundArgumentAllocationFailed);
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut values = exact_value_buffer(capacity, undefined)?;
        for index in 0..snapshot.item_count {
            let value = self.array_to_sorted_buffer_value(state, false, index)?;
            values[index as usize] = value;
        }
        let scratch = exact_value_buffer(capacity, undefined)?;
        let replacement = self.allocate_array_to_sorted_state(PendingArrayToSorted {
            receiver: snapshot.receiver,
            result: snapshot.result,
            comparator: snapshot.comparator,
            left_value: snapshot.left_value,
            right_value: snapshot.right_value,
            left_string: snapshot.left_string,
            retained: snapshot.retained,
            values,
            scratch,
            length: snapshot.length,
            item_count: snapshot.item_count,
            cursor: snapshot.cursor,
            width: snapshot.width,
            merge_start: snapshot.merge_start,
            left: snapshot.left,
            left_end: snapshot.left_end,
            right: snapshot.right,
            right_end: snapshot.right_end,
            destination: snapshot.destination,
            active_merge: snapshot.active_merge,
            copy: snapshot.copy,
        })?;
        self.root_array_to_sorted_state(site, replacement)?;
        Ok(replacement)
    }

    /// Returns externally-accounted buffer capacity without retaining a managed borrow.
    pub(super) fn array_to_sorted_buffer_len(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<usize, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(pending.values.len())
            })
        })
    }

    /// Reads one source or scratch buffer slot under a no-GC borrow.
    pub(super) fn array_to_sorted_buffer_value(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        scratch: bool,
        index: u64,
    ) -> Result<Value, ExecutionError> {
        let index = usize::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let buffer = if scratch {
                    &pending.scratch
                } else {
                    &pending.values
                };
                buffer
                    .get(index)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Updates one traced buffer slot and records the owner/value barrier.
    pub(super) fn set_array_to_sorted_buffer_value(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        scratch: bool,
        index: u64,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let index = usize::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let buffer = if scratch {
                    &mut pending.scratch
                } else {
                    &mut pending.values
                };
                *buffer
                    .get_mut(index)
                    .ok_or(ExecutionError::MissingNativeContinuation)? = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Updates one traced scalar edge and records its generational barrier.
    pub(super) fn set_array_to_sorted_value(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        field: impl FnOnce(&mut PendingArrayToSorted) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                *field(pending) = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Updates merge cursors without requiring a write barrier.
    pub(super) fn update_array_to_sorted_scalars(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        update: impl FnOnce(&mut PendingArrayToSorted),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }
}
