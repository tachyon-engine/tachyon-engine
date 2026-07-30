//! Property dispatch and managed-state access for Array reverse.

use super::*;

impl Isolate {
    /// Publishes a reverse parent around one Proxy/accessor-aware Get.
    pub(super) fn dispatch_array_reverse_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        stage: ArrayReverseStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayReverse>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_reverse_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_reverse_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes a reverse parent around one HasProperty operation.
    pub(super) fn dispatch_array_reverse_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        stage: ArrayReverseStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArrayReverse>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_reverse_parent(site, state, stage, key)?;
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
        let state = self.pending_array_reverse_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs Set(..., true) while preserving Proxy and setter behavior.
    pub(super) fn dispatch_array_reverse_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        stage: ArrayReverseStage,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<Option<GcRef<PendingArrayReverse>>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_reverse_parent(site, state, stage, value)?;
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
        self.pending_array_reverse_reference(rooted.first())
            .map(Some)
    }

    /// Performs DeletePropertyOrThrow for one absent counterpart.
    pub(super) fn dispatch_array_reverse_delete(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        stage: ArrayReverseStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<GcRef<PendingArrayReverse>>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_reverse_parent(site, state, stage, key)?;
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
        self.pending_array_reverse_reference(rooted.first())
            .map(Some)
    }

    /// Pushes one continuation that roots reverse state and operation data.
    fn push_array_reverse_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        stage: ArrayReverseStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_reverse(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the managed reverse state in the call destination.
    #[inline]
    pub(super) fn root_array_reverse_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates fixed reverse state under complete VM roots.
    pub(super) fn allocate_array_reverse_state(
        &mut self,
        pending: PendingArrayReverse,
    ) -> Result<GcRef<PendingArrayReverse>, ExecutionError> {
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
                self.types.pending_array_reverse,
                0,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked reverse-state reference from a managed Value.
    pub(crate) fn pending_array_reverse_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayReverse>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_reverse)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies state fields without retaining a no-GC borrow across a safepoint.
    pub(super) fn array_reverse_snapshot(
        &mut self,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<ArrayReverseSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_reverse)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayReverseSnapshot {
                    receiver: pending.receiver,
                    lower_value: pending.lower_value,
                    upper_value: pending.upper_value,
                    length: pending.length,
                    lower: pending.lower,
                    lower_present: pending.lower_present,
                    upper_present: pending.upper_present,
                })
            })
        })
    }

    /// Updates scalar reverse fields without requiring a write barrier.
    pub(super) fn update_array_reverse_scalars(
        &mut self,
        state: GcRef<PendingArrayReverse>,
        update: impl FnOnce(&mut PendingArrayReverse),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_reverse)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Clears pair-presence state before observing a new lower index.
    pub(super) fn reset_array_reverse_pair(
        &mut self,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        self.update_array_reverse_scalars(state, |pending| {
            pending.lower_present = false;
            pending.upper_present = false;
        })
    }

    /// Stores one presence result for the current pair.
    pub(super) fn set_array_reverse_presence(
        &mut self,
        state: GcRef<PendingArrayReverse>,
        lower: bool,
        present: bool,
    ) -> Result<(), ExecutionError> {
        self.update_array_reverse_scalars(state, |pending| {
            if lower {
                pending.lower_present = present;
            } else {
                pending.upper_present = present;
            }
        })
    }

    /// Stores one observed pair value and records its generational edge.
    pub(super) fn set_array_reverse_value(
        &mut self,
        state: GcRef<PendingArrayReverse>,
        lower: bool,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_reverse)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if lower {
                    pending.lower_value = value;
                } else {
                    pending.upper_value = value;
                }
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Advances the lower cursor only after both selected mutations succeed.
    pub(super) fn commit_array_reverse_pair(
        &mut self,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        self.update_array_reverse_scalars(state, |pending| pending.lower += 1)
    }
}
