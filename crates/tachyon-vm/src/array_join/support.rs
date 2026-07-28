//! Property dispatch and externally-accounted backing for Array join.

use super::*;

impl Isolate {
    /// Publishes a join parent around one Proxy/accessor-aware Get.
    pub(super) fn dispatch_array_join_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        stage: ArrayJoinStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayJoin>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_join_parent(site, state, stage, receiver)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(None);
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_join_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Pushes one continuation that roots join state and local data.
    fn push_array_join_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        stage: ArrayJoinStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_join(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots join state in the call destination before any safepoint.
    #[inline]
    pub(super) fn root_array_join_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates join state and charges both fixed UTF-16 backings.
    pub(super) fn allocate_array_join_state(
        &mut self,
        pending: PendingArrayJoin,
    ) -> Result<GcRef<PendingArrayJoin>, ExecutionError> {
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
                self.types.pending_array_join,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked join-state reference from a managed Value.
    pub(crate) fn pending_array_join_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayJoin>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_join)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies join scalar and traced fields without crossing a safepoint borrow.
    pub(super) fn array_join_snapshot(
        &mut self,
        state: GcRef<PendingArrayJoin>,
    ) -> Result<ArrayJoinSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_join)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayJoinSnapshot {
                    receiver: pending.receiver,
                    separator_argument: pending.separator_argument,
                    retained: pending.retained,
                    length: pending.length,
                    cursor: pending.cursor,
                    output_len: pending.output_len,
                    output_capacity: pending.output.len(),
                    locale: pending.locale,
                })
            })
        })
    }

    /// Updates join cursors without requiring a write barrier.
    pub(super) fn update_array_join_scalars(
        &mut self,
        state: GcRef<PendingArrayJoin>,
        update: impl FnOnce(&mut PendingArrayJoin),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_join)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Replaces the retained edge and records its old-to-young barrier.
    pub(super) fn set_array_join_retained(
        &mut self,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_join)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.retained = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Copies the installed separator into output, growing by replacement when needed.
    pub(super) fn append_array_join_separator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
    ) -> Result<GcRef<PendingArrayJoin>, ExecutionError> {
        let units = self.array_join_separator_units(state)?;
        self.append_array_join_units(site, state, &units)
    }

    /// Appends UTF-16 units into fixed backing after ensuring exact capacity.
    pub(super) fn append_array_join_units(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        units: &[u16],
    ) -> Result<GcRef<PendingArrayJoin>, ExecutionError> {
        let required = self
            .array_join_snapshot(state)?
            .output_len
            .checked_add(units.len())
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        let state = self.ensure_array_join_capacity(site, state, required)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_join)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let end = pending
                    .output_len
                    .checked_add(units.len())
                    .ok_or(ExecutionError::StringBufferAllocationFailed)?;
                pending.output[pending.output_len..end].copy_from_slice(units);
                pending.output_len = end;
                Ok(())
            })
        })?;
        Ok(state)
    }

    /// Allocates a larger externally-accounted state and copies committed output.
    fn ensure_array_join_capacity(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        required: usize,
    ) -> Result<GcRef<PendingArrayJoin>, ExecutionError> {
        let snapshot = self.array_join_snapshot(state)?;
        if required <= snapshot.output_capacity {
            return Ok(state);
        }
        let capacity =
            tuning::arrays::grown_array_join_capacity(snapshot.output_capacity, required)
                .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        let separator = self.array_join_separator_units(state)?.into_boxed_slice();
        let committed = self.array_join_output_units(state)?;
        let mut output = exact_array_join_buffer(capacity)?;
        output[..committed.len()].copy_from_slice(&committed);
        let replacement = self.allocate_array_join_state(PendingArrayJoin {
            receiver: snapshot.receiver,
            separator_argument: snapshot.separator_argument,
            retained: snapshot.retained,
            separator,
            output,
            length: snapshot.length,
            cursor: snapshot.cursor,
            output_len: snapshot.output_len,
            locale: snapshot.locale,
        })?;
        self.root_array_join_state(site, replacement)?;
        Ok(replacement)
    }

    /// Copies separator units without retaining a managed borrow.
    fn array_join_separator_units(
        &mut self,
        state: GcRef<PendingArrayJoin>,
    ) -> Result<Vec<u16>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_join)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(pending.separator.to_vec())
            })
        })
    }

    /// Copies committed output units without retaining a managed borrow.
    fn array_join_output_units(
        &mut self,
        state: GcRef<PendingArrayJoin>,
    ) -> Result<Vec<u16>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_join)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(pending.output[..pending.output_len].to_vec())
            })
        })
    }

    /// Allocates the final runtime string from committed UTF-16 output.
    pub(super) fn finish_array_join_output(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
    ) -> Result<(), ExecutionError> {
        let units = self.array_join_output_units(state)?;
        let string = JsString::try_from_owned_code_units(units)
            .map_err(ExecutionError::PropertyKeyString)?;
        let value = self.allocate_runtime_string(string)?;
        self.write(site.caller_base, site.destination, value)
    }
}
