//! Property dispatch and managed-state access for Array pop/shift.

use super::*;

impl Isolate {
    /// Publishes a typed parent around one Proxy/accessor-aware property Get.
    pub(super) fn dispatch_array_remove_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        stage: ArrayRemoveStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayRemove>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_remove_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_remove_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes a typed parent around one HasProperty operation.
    pub(super) fn dispatch_array_remove_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        stage: ArrayRemoveStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArrayRemove>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_remove_parent(site, state, stage, key)?;
        if let Err(error) = self.dispatch_has_property(site, receiver, key) {
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
        let state = self.pending_array_remove_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs Set(..., true), preserving setters and Proxy traps.
    pub(super) fn dispatch_array_remove_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        stage: ArrayRemoveStage,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_remove_parent(site, state, stage, value)?;
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
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_remove_reference(rooted.first())?;
        self.resume_array_remove(site, state, stage, value)
    }

    /// Performs DeletePropertyOrThrow for an ordinary object or Proxy.
    pub(super) fn dispatch_array_remove_delete(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        stage: ArrayRemoveStage,
        receiver: Value,
        key: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_remove_parent(site, state, stage, key)?;
        if let Err(error) =
            self.dispatch_delete_property(site, receiver, key, ProxyDeleteMode::Strict)
        {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_remove_reference(rooted.first())?;
        self.resume_array_remove(site, state, stage, boolean_value(true))
    }

    /// Pushes one continuation that roots both the state and operation value.
    fn push_array_remove_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        stage: ArrayRemoveStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_remove(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the managed state in the call destination before a safepoint.
    #[inline]
    pub(super) fn root_array_remove_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates one fixed-size removal state under the complete VM root set.
    pub(super) fn allocate_array_remove_state(
        &mut self,
        pending: PendingArrayRemove,
    ) -> Result<GcRef<PendingArrayRemove>, ExecutionError> {
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
            .try_allocate_external_with_gc(
                self.types.pending_array_remove,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked removal-state reference from a managed Value.
    pub(crate) fn pending_array_remove_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayRemove>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_remove)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies state fields so no managed borrow crosses an observable operation.
    pub(super) fn array_remove_snapshot(
        &mut self,
        state: GcRef<PendingArrayRemove>,
    ) -> Result<ArrayRemoveSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_remove)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayRemoveSnapshot {
                    receiver: pending.receiver,
                    retained: pending.retained,
                    length: pending.length,
                    cursor: pending.cursor,
                    shift: pending.shift,
                })
            })
        })
    }

    /// Updates cursor or length fields that do not require a write barrier.
    pub(super) fn update_array_remove_scalars(
        &mut self,
        state: GcRef<PendingArrayRemove>,
        update: impl FnOnce(&mut PendingArrayRemove),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_remove)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Stores the return value and records the generational write barrier.
    pub(super) fn set_array_remove_retained(
        &mut self,
        state: GcRef<PendingArrayRemove>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_array_remove)
                    .map(|pending| pending.retained = value)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}
