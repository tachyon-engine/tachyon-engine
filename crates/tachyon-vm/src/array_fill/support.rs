//! Property dispatch and managed-state access for Array fill.

use super::*;

impl Isolate {
    /// Publishes a fill parent around one Proxy/accessor-aware Get.
    pub(super) fn dispatch_array_fill_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        stage: ArrayFillStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayFill>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_fill_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_fill_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs Set(..., true) while preserving Proxy and setter behavior.
    pub(super) fn dispatch_array_fill_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        stage: ArrayFillStage,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<Option<GcRef<PendingArrayFill>>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_fill_parent(site, state, stage, value)?;
        if let Err(error) = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            key,
            value,
            ProxySetMode::ObjectAssign,
        ) {
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
        self.pending_array_fill_reference(rooted.first()).map(Some)
    }

    /// Pushes one continuation that roots fill state and operation data.
    fn push_array_fill_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        stage: ArrayFillStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_fill(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the managed fill state in the call destination.
    #[inline]
    pub(super) fn root_array_fill_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates fixed fill state under complete VM roots.
    pub(super) fn allocate_array_fill_state(
        &mut self,
        pending: PendingArrayFill,
    ) -> Result<GcRef<PendingArrayFill>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_array_fill,
                0,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked fill-state reference from a managed Value.
    pub(crate) fn pending_array_fill_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayFill>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_fill)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies state fields without retaining a no-GC borrow across a safepoint.
    pub(super) fn array_fill_snapshot(
        &mut self,
        state: GcRef<PendingArrayFill>,
    ) -> Result<ArrayFillSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_fill)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayFillSnapshot {
                    receiver: pending.receiver,
                    value: pending.value,
                    start_argument: pending.start_argument,
                    end_argument: pending.end_argument,
                    length: pending.length,
                    cursor: pending.cursor,
                    end: pending.end,
                })
            })
        })
    }

    /// Updates scalar fill fields without requiring a write barrier.
    pub(super) fn update_array_fill_scalars(
        &mut self,
        state: GcRef<PendingArrayFill>,
        update: impl FnOnce(&mut PendingArrayFill),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_fill)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Advances the cursor only after the indexed Set succeeds.
    pub(super) fn commit_array_fill_set(
        &mut self,
        state: GcRef<PendingArrayFill>,
    ) -> Result<(), ExecutionError> {
        self.update_array_fill_scalars(state, |pending| pending.cursor += 1)
    }
}
