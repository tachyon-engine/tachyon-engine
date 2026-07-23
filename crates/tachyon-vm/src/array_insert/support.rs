//! Property dispatch and managed-state access for Array push/unshift.

use super::*;

impl Isolate {
    /// Publishes an insertion parent around one Proxy/accessor-aware Get.
    pub(super) fn dispatch_array_insert_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        stage: ArrayInsertStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayInsert>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_insert_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_insert_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes an insertion parent around one HasProperty operation.
    pub(super) fn dispatch_array_insert_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        stage: ArrayInsertStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArrayInsert>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_insert_parent(site, state, stage, key)?;
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
        let state = self.pending_array_insert_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs Set(..., true), preserving setters and Proxy traps.
    pub(super) fn dispatch_array_insert_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        stage: ArrayInsertStage,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<Option<GcRef<PendingArrayInsert>>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_insert_parent(site, state, stage, value)?;
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
        let state = self.pending_array_insert_reference(rooted.first())?;
        Ok(Some(state))
    }

    /// Performs DeletePropertyOrThrow for an ordinary object or Proxy.
    pub(super) fn dispatch_array_insert_delete(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        stage: ArrayInsertStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<GcRef<PendingArrayInsert>>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_insert_parent(site, state, stage, key)?;
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
            return Ok(None);
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_insert_reference(rooted.first())?;
        Ok(Some(state))
    }

    /// Pushes one continuation that roots insertion state and operation data.
    fn push_array_insert_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        stage: ArrayInsertStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_insert(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the managed insertion state in the call destination.
    #[inline]
    pub(super) fn root_array_insert_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates one insertion state and exact item backing under VM roots.
    pub(super) fn allocate_array_insert_state(
        &mut self,
        pending: PendingArrayInsert,
    ) -> Result<GcRef<PendingArrayInsert>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_array_insert,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked insertion-state reference from a managed Value.
    pub(crate) fn pending_array_insert_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayInsert>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_insert)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies scalar and rooted fields without carrying a borrow across a safepoint.
    pub(super) fn array_insert_snapshot(
        &mut self,
        state: GcRef<PendingArrayInsert>,
    ) -> Result<ArrayInsertSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_insert)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayInsertSnapshot {
                    receiver: pending.receiver,
                    length: pending.length,
                    cursor: pending.cursor,
                    item_count: pending.items.len() as u64,
                    unshift: pending.unshift,
                })
            })
        })
    }

    /// Copies one immutable captured argument from the exact backing.
    pub(super) fn array_insert_item(
        &mut self,
        state: GcRef<PendingArrayInsert>,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_array_insert)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .items
                    .get(index)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Updates scalar cursor/length fields without requiring a write barrier.
    pub(super) fn update_array_insert_scalars(
        &mut self,
        state: GcRef<PendingArrayInsert>,
        update: impl FnOnce(&mut PendingArrayInsert),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_insert)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Stores one moved element and records the generational write barrier.
    pub(super) fn set_array_insert_retained(
        &mut self,
        state: GcRef<PendingArrayInsert>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_array_insert)
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
