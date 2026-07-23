//! Property dispatch, managed-state access, and numeric helpers for splice.

use super::*;

impl Isolate {
    /// Publishes a splice parent around one Proxy/accessor-aware property Get.
    pub(super) fn dispatch_array_splice_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        stage: ArraySpliceStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArraySplice>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_splice_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_splice_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes a splice parent around HasProperty.
    pub(super) fn dispatch_array_splice_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        stage: ArraySpliceStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArraySplice>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_splice_parent(site, state, stage, key)?;
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
        let state = self.pending_array_splice_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs Set(..., true), preserving setters and Proxy traps.
    pub(super) fn dispatch_array_splice_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        stage: ArraySpliceStage,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_splice_parent(site, state, stage, value)?;
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
        let state = self.pending_array_splice_reference(rooted.first())?;
        self.resume_array_splice(site, state, stage, value, value)
    }

    /// Performs DeletePropertyOrThrow for an ordinary object or Proxy.
    pub(super) fn dispatch_array_splice_delete(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        stage: ArraySpliceStage,
        receiver: Value,
        key: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_splice_parent(site, state, stage, key)?;
        let outcome = self.dispatch_delete_property(site, receiver, key, ProxyDeleteMode::Strict);
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
        let state = self.pending_array_splice_reference(rooted.first())?;
        self.resume_array_splice(site, state, stage, boolean_value(true), key)
    }

    /// Performs CreateDataPropertyOrThrow on a Proxy species result.
    pub(super) fn dispatch_array_splice_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        stage: ArraySpliceStage,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_splice_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_splice_reference(rooted.first())?;
        self.finish_array_splice_copy(site, state)
    }

    /// Pushes one typed splice parent that also roots operation-specific retained data.
    pub(super) fn push_array_splice_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        stage: ArraySpliceStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_splice(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the state in the caller destination before any allocation-capable operation.
    #[inline]
    pub(super) fn root_array_splice_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates the variable-size item list under the complete VM root set.
    pub(super) fn allocate_array_splice_state(
        &mut self,
        pending: PendingArraySplice,
    ) -> Result<GcRef<PendingArraySplice>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_array_splice,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Validates a managed Value and recovers the typed splice state reference.
    pub(crate) fn pending_array_splice_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArraySplice>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_splice)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Takes a scalar-only snapshot so no managed borrow crosses a safepoint.
    pub(super) fn array_splice_snapshot(
        &mut self,
        state: GcRef<PendingArraySplice>,
    ) -> Result<ArraySpliceSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_splice)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArraySpliceSnapshot {
                    receiver: pending.receiver,
                    result: pending.result,
                    len: pending.len,
                    start: pending.start,
                    delete_count: pending.delete_count,
                    new_len: pending.new_len,
                    cursor: pending.cursor,
                    argument_count: pending.argument_count,
                    item_count: pending.items.len() as u64,
                })
            })
        })
    }

    /// Copies one item Value from the immutable exact-size item backing.
    pub(super) fn array_splice_item(
        &mut self,
        state: GcRef<PendingArraySplice>,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_array_splice)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .items
                    .get(index)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Reads the explicit deleteCount argument.
    pub(super) fn array_splice_delete_argument(
        &mut self,
        state: GcRef<PendingArraySplice>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_array_splice)
                    .map(|pending| pending.delete_argument)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Updates scalar cursor/count fields without requiring a write barrier.
    pub(super) fn update_array_splice_scalars(
        &mut self,
        state: GcRef<PendingArraySplice>,
        update: impl FnOnce(&mut PendingArraySplice),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_splice)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates one traced state edge and records the generational barrier.
    pub(super) fn set_array_splice_value(
        &mut self,
        state: GcRef<PendingArraySplice>,
        field: impl FnOnce(&mut PendingArraySplice) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_splice)
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

    /// Skips an ordinary deleted-result hole run while preserving Proxy observation.
    pub(super) fn skip_array_splice_copy_holes(
        &mut self,
        state: GcRef<PendingArraySplice>,
        current: u64,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let end = snapshot.start + snapshot.delete_count;
        let candidate = self.next_splice_property(snapshot.receiver, current + 1, end, false)?;
        self.update_array_splice_scalars(state, |pending| {
            pending.cursor = candidate.unwrap_or(end) - pending.start;
        })
    }

    /// Finds the nearest ordinary indexed property and declines to skip across any Proxy.
    fn next_splice_property(
        &mut self,
        receiver: Value,
        lower: u64,
        upper: u64,
        reverse: bool,
    ) -> Result<Option<u64>, ExecutionError> {
        let mut candidate = None;
        let mut current = receiver;
        loop {
            if self.is_proxy_value(current) {
                return Ok(if reverse {
                    upper.checked_sub(1)
                } else {
                    Some(lower)
                });
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            let mut keys = self.ordinary_own_property_keys(current, snapshot)?;
            while let Some(entry) = keys.next_entry() {
                let Some(index) = entry
                    .key
                    .atom()
                    .and_then(|atom| self.atoms.get(atom))
                    .and_then(|string| splice_safe_integer_index(string.as_view()))
                    .filter(|index| *index >= lower && *index < upper)
                else {
                    continue;
                };
                candidate = Some(candidate.map_or(index, |old: u64| {
                    if reverse {
                        old.max(index)
                    } else {
                        old.min(index)
                    }
                }));
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(candidate);
            }
            current = snapshot.prototype;
        }
    }
}

impl ArraySpliceSnapshot {
    /// Reads the start argument without exposing the managed state borrow.
    pub(super) fn start_argument(
        self,
        isolate: &mut Isolate,
        state: GcRef<PendingArraySplice>,
    ) -> Result<Value, ExecutionError> {
        isolate.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, isolate.types.pending_array_splice)
                    .map(|pending| pending.start_argument)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}

#[inline(always)]
pub(super) fn splice_integer(number: f64) -> f64 {
    if number.is_nan() || number == 0.0 {
        0.0
    } else if number.is_infinite() {
        number
    } else {
        number.trunc()
    }
}

#[inline(always)]
pub(super) fn relative_start(length: u64, relative: f64) -> u64 {
    if relative <= -(length as f64) {
        0
    } else if relative < 0.0 {
        (length as f64 + relative) as u64
    } else if relative >= length as f64 {
        length
    } else {
        relative as u64
    }
}

#[inline(always)]
pub(super) fn splice_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    let integer = splice_integer(number);
    if integer <= 0.0 {
        Ok(0)
    } else if integer >= MAX_SAFE_INTEGER as f64 {
        Ok(MAX_SAFE_INTEGER)
    } else {
        Ok(integer as u64)
    }
}

#[inline(always)]
pub(super) fn splice_move_indices(snapshot: ArraySpliceSnapshot) -> (u64, u64) {
    if snapshot.item_count < snapshot.delete_count {
        (
            snapshot.cursor + snapshot.delete_count,
            snapshot.cursor + snapshot.item_count,
        )
    } else {
        (
            snapshot.cursor + snapshot.delete_count - 1,
            snapshot.cursor + snapshot.item_count - 1,
        )
    }
}

/// Parses canonical decimal keys through the complete safe-integer range.
fn splice_safe_integer_index(string: JsStringView<'_>) -> Option<u64> {
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
