//! Property dispatch, managed state, and numeric helpers for slice.

use super::*;

impl Isolate {
    /// Publishes a slice parent around one Proxy/accessor-aware property Get.
    pub(super) fn dispatch_array_slice_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        stage: ArraySliceStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArraySlice>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_slice_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_slice_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes a slice parent around one HasProperty operation.
    pub(super) fn dispatch_array_slice_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArraySlice>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_slice_parent(site, state, ArraySliceStage::ElementHas, key)?;
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
        let state = self.pending_array_slice_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs CreateDataPropertyOrThrow on a Proxy species result.
    pub(super) fn dispatch_array_slice_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_slice_parent(site, state, ArraySliceStage::ElementDefine, receiver)?;
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
        let state = self.pending_array_slice_reference(rooted.first())?;
        self.finish_array_slice_element(site, state)
    }

    /// Performs the final Set(A, "length", n, true).
    pub(super) fn dispatch_array_slice_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_slice_parent(site, state, ArraySliceStage::FinalLength, value)?;
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
        let state = self.pending_array_slice_reference(rooted.first())?;
        self.finish_array_slice(site, state)
    }

    /// Pushes one typed slice parent with one operation-specific retained value.
    pub(super) fn push_array_slice_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        stage: ArraySliceStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_slice(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the state in the caller destination before allocation-capable work.
    #[inline]
    pub(super) fn root_array_slice_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates the fixed-size slice state under the complete root set.
    pub(super) fn allocate_array_slice_state(
        &mut self,
        pending: PendingArraySlice,
    ) -> Result<GcRef<PendingArraySlice>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_array_slice,
                0,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked typed slice-state reference from a managed Value.
    pub(crate) fn pending_array_slice_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArraySlice>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_slice)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Takes a scalar-only snapshot so no managed borrow crosses a safepoint.
    pub(super) fn array_slice_snapshot(
        &mut self,
        state: GcRef<PendingArraySlice>,
    ) -> Result<ArraySliceSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_slice)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArraySliceSnapshot {
                    receiver: pending.receiver,
                    result: pending.result,
                    length: pending.length,
                    start: pending.start,
                    final_index: pending.final_index,
                    source_index: pending.source_index,
                    target_index: pending.target_index,
                })
            })
        })
    }

    /// Reads the captured start or end argument without exposing a managed borrow.
    pub(super) fn array_slice_argument(
        &mut self,
        state: GcRef<PendingArraySlice>,
        start: bool,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_slice)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(if start {
                    pending.start_argument
                } else {
                    pending.end_argument
                })
            })
        })
    }

    /// Updates scalar cursor/count fields without requiring a write barrier.
    pub(super) fn update_array_slice_scalars(
        &mut self,
        state: GcRef<PendingArraySlice>,
        update: impl FnOnce(&mut PendingArraySlice),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_slice)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates one traced state edge and records the generational barrier.
    pub(super) fn set_array_slice_value(
        &mut self,
        state: GcRef<PendingArraySlice>,
        field: impl FnOnce(&mut PendingArraySlice) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_slice)
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

    /// Advances source and target cursors together, preserving holes in the result.
    pub(super) fn advance_array_slice_cursor(
        &mut self,
        state: GcRef<PendingArraySlice>,
    ) -> Result<(), ExecutionError> {
        self.update_array_slice_scalars(state, |pending| {
            pending.source_index += 1;
            pending.target_index += 1;
        })
    }
}

#[inline(always)]
pub(super) fn slice_integer(number: f64) -> f64 {
    if number.is_nan() || number == 0.0 {
        0.0
    } else if number.is_infinite() {
        number
    } else {
        number.trunc()
    }
}

#[inline(always)]
pub(super) fn relative_slice_index(length: u64, relative: f64) -> u64 {
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
pub(super) fn slice_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    let integer = slice_integer(number);
    if integer <= 0.0 {
        Ok(0)
    } else if integer >= MAX_SAFE_INTEGER as f64 {
        Ok(MAX_SAFE_INTEGER)
    } else {
        Ok(integer as u64)
    }
}
