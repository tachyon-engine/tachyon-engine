//! Property dispatch, managed frame backing, and sparse scanning for flat.

use super::*;
use crate::tuning::arrays::{
    ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD, INITIAL_ARRAY_FLAT_FRAME_CAPACITY,
    grown_array_flat_frame_capacity,
};

impl Isolate {
    /// Publishes a flat parent around one Proxy/accessor-aware property Get.
    pub(super) fn dispatch_array_flat_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        stage: ArrayFlatStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingArrayFlat>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_flat_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_flat_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Publishes a flat parent around one HasProperty operation.
    pub(super) fn dispatch_array_flat_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        stage: ArrayFlatStage,
        receiver: Value,
        key: Value,
    ) -> Result<Option<(GcRef<PendingArrayFlat>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_flat_parent(site, state, stage, key)?;
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
        let state = self.pending_array_flat_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Performs CreateDataPropertyOrThrow on a Proxy species result.
    pub(super) fn dispatch_array_flat_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_flat_parent(site, state, ArrayFlatStage::Define, receiver)?;
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
        let state = self.pending_array_flat_reference(rooted.first())?;
        self.finish_array_flat_define(site, state)
    }

    /// Pushes one typed flat parent with one operation-specific retained value.
    pub(super) fn push_array_flat_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        stage: ArrayFlatStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_flat(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots the state in the caller destination before allocation-capable work.
    #[inline]
    pub(super) fn root_array_flat_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates externally-accounted fixed frame backing under the complete root set.
    pub(super) fn allocate_array_flat_state(
        &mut self,
        pending: PendingArrayFlat,
    ) -> Result<GcRef<PendingArrayFlat>, ExecutionError> {
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
                self.types.pending_array_flat,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked typed flat state reference from a managed Value.
    pub(crate) fn pending_array_flat_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayFlat>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_flat)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Takes a scalar-only snapshot so no managed borrow crosses a safepoint.
    pub(super) fn array_flat_snapshot(
        &mut self,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<ArrayFlatSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_flat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayFlatSnapshot {
                    receiver: pending.receiver,
                    result: pending.result,
                    retained: pending.retained,
                    constructor: pending.constructor,
                    depth_argument: pending.depth_argument,
                    source: pending.source,
                    length: pending.length,
                    index: pending.index,
                    target_index: pending.target_index,
                    depth: pending.depth,
                    frame_count: pending.frame_count,
                    infinite_depth: pending.infinite_depth,
                })
            })
        })
    }

    /// Updates scalar cursor/count fields without requiring a write barrier.
    pub(super) fn update_array_flat_scalars(
        &mut self,
        state: GcRef<PendingArrayFlat>,
        update: impl FnOnce(&mut PendingArrayFlat),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_flat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates one traced state edge and records the generational barrier.
    pub(super) fn set_array_flat_value(
        &mut self,
        state: GcRef<PendingArrayFlat>,
        field: impl FnOnce(&mut PendingArrayFlat) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_flat)
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

    /// Replaces the bootstrap state with an educated fixed frame reservation.
    pub(super) fn prepare_array_flat_frames(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        depth: u64,
        infinite_depth: bool,
    ) -> Result<GcRef<PendingArrayFlat>, ExecutionError> {
        let snapshot = self.array_flat_snapshot(state)?;
        let requested = if infinite_depth {
            INITIAL_ARRAY_FLAT_FRAME_CAPACITY
        } else {
            usize::try_from(depth)
                .unwrap_or(usize::MAX)
                .min(INITIAL_ARRAY_FLAT_FRAME_CAPACITY)
        };
        let frames = exact_array_flat_frames(requested)?;
        let replacement = self.allocate_array_flat_state(PendingArrayFlat {
            receiver: snapshot.receiver,
            result: snapshot.result,
            retained: snapshot.retained,
            constructor: snapshot.constructor,
            depth_argument: snapshot.depth_argument,
            source: snapshot.source,
            frames,
            length: snapshot.length,
            index: snapshot.index,
            target_index: snapshot.target_index,
            depth,
            frame_count: 0,
            infinite_depth,
        })?;
        self.root_array_flat_state(site, replacement)?;
        Ok(replacement)
    }

    /// Pushes the current traversal, replacing immutable backing when it is full.
    pub(super) fn push_array_flat_frame(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayFlat>,
    ) -> Result<GcRef<PendingArrayFlat>, ExecutionError> {
        let mut snapshot = self.array_flat_snapshot(state)?;
        if snapshot.frame_count >= self.array_flat_frame_capacity(state)? {
            state = self.grow_array_flat_frames(site, state, snapshot)?;
            snapshot = self.array_flat_snapshot(state)?;
        }
        let frame = ArrayFlatFrame {
            source: snapshot.source,
            length: snapshot.length,
            index: snapshot.index,
            depth: snapshot.depth,
            infinite_depth: snapshot.infinite_depth,
        };
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_flat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.frames[pending.frame_count] = frame;
                pending.frame_count += 1;
                Ok(())
            })
        })?;
        self.root_array_flat_state(site, state)?;
        Ok(state)
    }

    /// Restores the most recently suspended source traversal.
    pub(super) fn pop_array_flat_frame(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<GcRef<PendingArrayFlat>, ExecutionError> {
        let frame_count = self.array_flat_snapshot(state)?.frame_count;
        let frame = self.array_flat_frame(state, frame_count - 1)?;
        self.set_array_flat_value(state, |pending| &mut pending.source, frame.source)?;
        self.update_array_flat_scalars(state, |pending| {
            pending.frame_count -= 1;
            pending.length = frame.length;
            pending.index = frame.index;
            pending.depth = frame.depth;
            pending.infinite_depth = frame.infinite_depth;
        })?;
        self.root_array_flat_state(site, state)?;
        Ok(state)
    }

    /// Replaces a full frame state with doubled, externally-accounted backing.
    fn grow_array_flat_frames(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        snapshot: ArrayFlatSnapshot,
    ) -> Result<GcRef<PendingArrayFlat>, ExecutionError> {
        let current = self.array_flat_frame_capacity(state)?;
        let capacity = grown_array_flat_frame_capacity(current.max(1))
            .ok_or(ExecutionError::RegisterAllocationFailed)?;
        let mut frames = exact_array_flat_frames(capacity)?;
        for (index, slot) in frames[..snapshot.frame_count].iter_mut().enumerate() {
            *slot = self.array_flat_frame(state, index)?;
        }
        let replacement = self.allocate_array_flat_state(PendingArrayFlat {
            receiver: snapshot.receiver,
            result: snapshot.result,
            retained: snapshot.retained,
            constructor: snapshot.constructor,
            depth_argument: snapshot.depth_argument,
            source: snapshot.source,
            frames,
            length: snapshot.length,
            index: snapshot.index,
            target_index: snapshot.target_index,
            depth: snapshot.depth,
            frame_count: snapshot.frame_count,
            infinite_depth: snapshot.infinite_depth,
        })?;
        self.root_array_flat_state(site, replacement)?;
        Ok(replacement)
    }

    /// Reads immutable frame capacity without retaining a borrow.
    fn array_flat_frame_capacity(
        &mut self,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<usize, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_array_flat)
                    .map(|pending| pending.frames.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Reads one initialized frame without retaining a managed borrow.
    fn array_flat_frame(
        &mut self,
        state: GcRef<PendingArrayFlat>,
        index: usize,
    ) -> Result<ArrayFlatFrame, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_flat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending
                    .frames
                    .get(index)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Applies ToLength to a primitive numeric conversion result.
    pub(super) fn array_flat_to_length(&mut self, value: Value) -> Result<u64, ExecutionError> {
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
    pub(super) fn skip_array_flat_holes(
        &mut self,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_flat_snapshot(state)?;
        if snapshot.length.saturating_sub(snapshot.index) <= ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD {
            return Ok(());
        }
        let Some(next) =
            self.next_array_flat_candidate(snapshot.source, snapshot.index, snapshot.length)?
        else {
            return Ok(());
        };
        self.update_array_flat_scalars(state, |pending| pending.index = next)
    }

    /// Finds the next possible ordinary numeric property in a prototype chain.
    fn next_array_flat_candidate(
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
                    .and_then(|string| safe_integer_property_index(string.as_view()))
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

/// Allocates one exact immutable frame backing initialized with non-root placeholders.
fn exact_array_flat_frames(capacity: usize) -> Result<Box<[ArrayFlatFrame]>, ExecutionError> {
    let undefined = Value::from_immediate(Immediate::Undefined);
    let empty = ArrayFlatFrame {
        source: undefined,
        length: 0,
        index: 0,
        depth: 0,
        infinite_depth: false,
    };
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
    frames.resize(capacity, empty);
    Ok(frames.into_boxed_slice())
}
