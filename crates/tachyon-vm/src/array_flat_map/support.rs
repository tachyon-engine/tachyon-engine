//! Property dispatch, managed-state access, and sparse scanning for flatMap.

use super::*;
use crate::tuning::arrays::ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD;

impl Isolate {
    /// Publishes a flatMap parent around one Proxy/accessor-aware property Get.
    pub(super) fn dispatch_array_flat_map_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        stage: ArrayFlatMapStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayFlatMap>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_flat_map_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_flat_map_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes a flatMap parent around one HasProperty operation.
    pub(super) fn dispatch_array_flat_map_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        stage: ArrayFlatMapStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArrayFlatMap>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_flat_map_parent(site, state, stage, key)?;
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
        let state = self.pending_array_flat_map_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs CreateDataPropertyOrThrow on a Proxy species result.
    pub(super) fn dispatch_array_flat_map_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_flat_map_parent(site, state, ArrayFlatMapStage::Define, receiver)?;
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
        let state = self.pending_array_flat_map_reference(rooted.first())?;
        self.finish_array_flat_map_define(site, state)
    }

    /// Pushes one typed flatMap parent with one operation-specific retained value.
    pub(super) fn push_array_flat_map_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        stage: ArrayFlatMapStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_flat_map(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the state in the caller destination before allocation-capable work.
    #[inline]
    pub(super) fn root_array_flat_map_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates fixed-size flatMap state under the complete VM root set.
    pub(super) fn allocate_array_flat_map_state(
        &mut self,
        pending: PendingArrayFlatMap,
    ) -> Result<GcRef<PendingArrayFlatMap>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_array_flat_map,
                0,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked typed flatMap state reference from a managed Value.
    pub(crate) fn pending_array_flat_map_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayFlatMap>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_flat_map)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Takes a scalar-only snapshot so no managed borrow crosses a safepoint.
    pub(super) fn array_flat_map_snapshot(
        &mut self,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<ArrayFlatMapSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_flat_map)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayFlatMapSnapshot {
                    receiver: pending.receiver,
                    callback: pending.callback,
                    this_argument: pending.this_argument,
                    result: pending.result,
                    current: pending.current,
                    source_length: pending.source_length,
                    source_index: pending.source_index,
                    inner_length: pending.inner_length,
                    inner_index: pending.inner_index,
                    target_index: pending.target_index,
                    inner_active: pending.inner_active,
                })
            })
        })
    }

    /// Updates scalar cursor/count fields without requiring a write barrier.
    pub(super) fn update_array_flat_map_scalars(
        &mut self,
        state: GcRef<PendingArrayFlatMap>,
        update: impl FnOnce(&mut PendingArrayFlatMap),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_flat_map)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates one traced state edge and records the generational barrier.
    pub(super) fn set_array_flat_map_value(
        &mut self,
        state: GcRef<PendingArrayFlatMap>,
        field: impl FnOnce(&mut PendingArrayFlatMap) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_flat_map)
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

    /// Applies ToLength to a primitive numeric conversion result.
    pub(super) fn array_flat_map_to_length(&mut self, value: Value) -> Result<u64, ExecutionError> {
        let number =
            numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
            return Ok(MAX_SAFE_INTEGER);
        }
        Ok(number.floor() as u64)
    }

    /// Fast-forwards across a long ordinary hole run without crossing a Proxy boundary.
    pub(super) fn skip_array_flat_map_holes(
        &mut self,
        state: GcRef<PendingArrayFlatMap>,
        inner: bool,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_flat_map_snapshot(state)?;
        let (receiver, cursor, length) = if inner {
            (
                snapshot.current,
                snapshot.inner_index,
                snapshot.inner_length,
            )
        } else {
            (
                snapshot.receiver,
                snapshot.source_index,
                snapshot.source_length,
            )
        };
        if length.saturating_sub(cursor) <= ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD {
            return Ok(());
        }
        let Some(next) = self.next_flat_map_candidate(receiver, cursor, length)? else {
            return Ok(());
        };
        self.update_array_flat_map_scalars(state, |pending| {
            if inner {
                pending.inner_index = next;
            } else {
                pending.source_index = next;
            }
        })
    }

    /// Finds the next possible ordinary numeric property in a prototype chain.
    fn next_flat_map_candidate(
        &mut self,
        receiver: Value,
        lower: u64,
        upper: u64,
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
                    .and_then(|string| flat_map_safe_integer_index(string.as_view()))
                    .filter(|index| lower <= *index && *index < upper)
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
        Ok(Some(candidate.unwrap_or(upper)))
    }
}

/// Parses canonical safe-integer property names for ordinary sparse skipping.
fn flat_map_safe_integer_index(string: JsStringView<'_>) -> Option<u64> {
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
