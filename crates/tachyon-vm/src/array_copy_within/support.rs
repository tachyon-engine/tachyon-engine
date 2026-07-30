//! Property dispatch and managed-state access for Array copyWithin.

use super::*;

impl Isolate {
    /// Publishes the native parent around one Proxy/accessor-aware Get.
    pub(super) fn dispatch_array_copy_within_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        stage: ArrayCopyWithinStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayCopyWithin>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_copy_within_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_copy_within_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes the native parent around one HasProperty operation.
    pub(super) fn dispatch_array_copy_within_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        stage: ArrayCopyWithinStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArrayCopyWithin>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_copy_within_parent(site, state, stage, key)?;
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
        let state = self.pending_array_copy_within_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs Set(O, key, value, true), preserving Proxy and setter behavior.
    pub(super) fn dispatch_array_copy_within_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        stage: ArrayCopyWithinStage,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<Option<GcRef<PendingArrayCopyWithin>>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_copy_within_parent(site, state, stage, value)?;
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
        self.pending_array_copy_within_reference(rooted.first())
            .map(Some)
    }

    /// Performs DeletePropertyOrThrow for the current destination.
    pub(super) fn dispatch_array_copy_within_delete(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        stage: ArrayCopyWithinStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<GcRef<PendingArrayCopyWithin>>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_copy_within_parent(site, state, stage, key)?;
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
        self.pending_array_copy_within_reference(rooted.first())
            .map(Some)
    }

    /// Dispatches ToPrimitive while rooting the pending copyWithin state.
    pub(super) fn dispatch_array_copy_within_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.dispatch_object_primitive_conversion(
            consumer,
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
            value,
            site.call_site,
        )
    }

    /// Pushes one continuation which roots state and operation-specific data.
    fn push_array_copy_within_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        stage: ArrayCopyWithinStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_copy_within(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the managed state in the call destination register.
    #[inline]
    pub(super) fn root_array_copy_within_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates fixed copyWithin state under all VM roots.
    pub(super) fn allocate_array_copy_within_state(
        &mut self,
        pending: PendingArrayCopyWithin,
    ) -> Result<GcRef<PendingArrayCopyWithin>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_array_copy_within,
                0,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked copyWithin state reference from a managed Value.
    pub(crate) fn pending_array_copy_within_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayCopyWithin>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_copy_within)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies fields without retaining a no-GC borrow across a safepoint.
    pub(super) fn array_copy_within_snapshot(
        &mut self,
        state: GcRef<PendingArrayCopyWithin>,
    ) -> Result<ArrayCopyWithinSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_copy_within)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayCopyWithinSnapshot {
                    receiver: pending.receiver,
                    target_argument: pending.target_argument,
                    start_argument: pending.start_argument,
                    end_argument: pending.end_argument,
                    length: pending.length,
                    from: pending.from,
                    to: pending.to,
                    count: pending.count,
                })
            })
        })
    }

    /// Updates scalar traversal state without a write barrier.
    pub(super) fn update_array_copy_within_scalars(
        &mut self,
        state: GcRef<PendingArrayCopyWithin>,
        update: impl FnOnce(&mut PendingArrayCopyWithin),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_copy_within)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Retains a moved value and records its generational edge.
    pub(super) fn set_array_copy_within_retained(
        &mut self,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_array_copy_within)
                    .map(|pending| pending.retained = value)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Advances from/to and decrements count only after mutation succeeds.
    pub(super) fn commit_array_copy_within_move(
        &mut self,
        state: GcRef<PendingArrayCopyWithin>,
    ) -> Result<(), ExecutionError> {
        self.update_array_copy_within_scalars(state, |pending| {
            if pending.direction < 0 {
                pending.from = pending.from.saturating_sub(1);
                pending.to = pending.to.saturating_sub(1);
            } else {
                pending.from += 1;
                pending.to += 1;
            }
            pending.count -= 1;
        })
    }
}
