//! Property dispatch and managed-state access for concat.

use super::*;

impl Isolate {
    /// Publishes a concat parent around one Proxy/accessor-aware property Get.
    pub(super) fn dispatch_array_concat_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayConcat>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_concat_parent(site, state, stage, receiver)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key);
        if let Err(error) = outcome {
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
        let state = self.pending_array_concat_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes a concat parent around HasProperty.
    pub(super) fn dispatch_array_concat_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArrayConcat>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_concat_parent(site, state, stage, key)?;
        let outcome = self.dispatch_has_property(site, receiver, key);
        if let Err(error) = outcome {
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
        let state = self.pending_array_concat_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs CreateDataPropertyOrThrow on a Proxy species result.
    pub(super) fn dispatch_array_concat_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_concat_parent(site, state, stage, receiver)?;
        let outcome =
            self.dispatch_proxy_define(site, receiver, key, descriptor, ProxyDefineMode::Object);
        if let Err(error) = outcome {
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
        let state = self.pending_array_concat_reference(rooted.first())?;
        match stage {
            ArrayConcatStage::ElementDefine => self.finish_array_concat_element(site, state),
            ArrayConcatStage::ValueDefine => self.finish_array_concat_source(site, state),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Performs Set(..., true), preserving setters and Proxy traps.
    pub(super) fn dispatch_array_concat_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_concat_parent(site, state, stage, value)?;
        let outcome = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            key,
            value,
            ProxySetMode::ObjectAssign,
        );
        if let Err(error) = outcome {
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
        let state = self.pending_array_concat_reference(rooted.first())?;
        self.finish_array_concat(site, state)
    }

    /// Pushes one typed concat parent that roots operation-specific data.
    pub(super) fn push_array_concat_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_concat(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the state in the caller destination before an allocation-capable operation.
    #[inline]
    pub(super) fn root_array_concat_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates the exact-size captured argument list under the complete root set.
    pub(super) fn allocate_array_concat_state(
        &mut self,
        pending: PendingArrayConcat,
    ) -> Result<GcRef<PendingArrayConcat>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_array_concat,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Validates a managed Value and recovers the typed concat state reference.
    pub(crate) fn pending_array_concat_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayConcat>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_concat)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Takes a scalar-only snapshot so no managed borrow crosses a safepoint.
    pub(super) fn array_concat_snapshot(
        &mut self,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<ArrayConcatSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_concat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let source_count = u32::try_from(pending.arguments.len())
                    .ok()
                    .and_then(|count| count.checked_add(1))
                    .ok_or(ExecutionError::RegisterAllocationFailed)?;
                Ok(ArrayConcatSnapshot {
                    receiver: pending.receiver,
                    result: pending.result,
                    current: pending.current,
                    source_index: pending.source_index,
                    source_count,
                    source_length: pending.source_length,
                    element_index: pending.element_index,
                    next_index: pending.next_index,
                })
            })
        })
    }

    /// Reads one captured argument without exposing a managed borrow.
    pub(super) fn array_concat_argument(
        &mut self,
        state: GcRef<PendingArrayConcat>,
        index: u32,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_array_concat)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .arguments
                    .get(index as usize)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Updates scalar cursor fields without requiring a write barrier.
    pub(super) fn update_array_concat_scalars(
        &mut self,
        state: GcRef<PendingArrayConcat>,
        update: impl FnOnce(&mut PendingArrayConcat),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_concat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates one traced state edge and records the generational barrier.
    pub(super) fn set_array_concat_value(
        &mut self,
        state: GcRef<PendingArrayConcat>,
        field: impl FnOnce(&mut PendingArrayConcat) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_concat)
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

    /// Skips an ordinary hole run while preserving Proxy observation.
    pub(super) fn skip_array_concat_holes(
        &mut self,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_concat_snapshot(state)?;
        let lower = snapshot.element_index + 1;
        let candidate =
            self.next_concat_property(snapshot.current, lower, snapshot.source_length)?;
        self.update_array_concat_scalars(state, |pending| {
            pending.element_index = candidate.unwrap_or(pending.source_length);
        })
    }

    /// Finds the next ordinary indexed property and declines to skip across a Proxy.
    fn next_concat_property(
        &mut self,
        receiver: Value,
        lower: u64,
        upper: u64,
    ) -> Result<Option<u64>, ExecutionError> {
        let mut candidate = None;
        let mut current = receiver;
        loop {
            if self.is_proxy_value(current) {
                return Ok(Some(lower));
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            let mut keys = self.ordinary_own_property_keys(current, snapshot)?;
            while let Some(entry) = keys.next_entry() {
                let Some(index) = entry
                    .key
                    .atom()
                    .and_then(|atom| self.atoms.get(atom))
                    .and_then(|string| concat_safe_integer_index(string.as_view()))
                    .filter(|index| *index >= lower && *index < upper)
                else {
                    continue;
                };
                candidate = Some(candidate.map_or(index, |old: u64| old.min(index)));
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(candidate);
            }
            current = snapshot.prototype;
        }
    }
}

/// Parses canonical decimal property keys through the safe-integer range.
fn concat_safe_integer_index(string: JsStringView<'_>) -> Option<u64> {
    let length = string.len();
    if length == 0 || length > 16 {
        return None;
    }
    let first = string.code_unit_at(0)?;
    if first == u16::from(b'0') {
        return (length == 1).then_some(0);
    }
    if !(u16::from(b'1')..=u16::from(b'9')).contains(&first) {
        return None;
    }
    let mut value = u64::from(first - u16::from(b'0'));
    for index in 1..length {
        let unit = string.code_unit_at(index)?;
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&unit) {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(unit - u16::from(b'0')))?;
        if value > MAX_SAFE_INTEGER {
            return None;
        }
    }
    Some(value)
}
