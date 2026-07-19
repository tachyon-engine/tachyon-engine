//! Allocation planning, capacity preflight, and transactional object publication.

use super::{AllocationSpace, GcExternalMemory, Heap, HeapAllocationError, ManagedAllocationError};
use crate::{
    GC_HEADER_EXTERNAL_BYTES_FLAG, GcAllocationPolicy, GcRef, GcType, GcTypeId,
    LargeAllocationError, LargeReclaim, ObjectLayout, RawHeapRef, SPAN_SIZE_BYTES,
    SmallObjectLayout, SpanId, SpanSpace, SpanTableError, Trace,
    trigger::{CollectionAction, CollectionRequest},
    tuning::SMALL_SIZE_CLASSES,
};

#[derive(Clone, Copy)]
struct AllocationPlan {
    space: AllocationSpace,
    class_index: Option<usize>,
    allocation_bytes: usize,
    required_span_storage_bytes: usize,
    external_bytes: usize,
}

struct AllocationRequest<T: Trace> {
    object_type: GcType<T>,
    flags: u16,
    aux: u32,
    value: T,
    space: AllocationSpace,
    external_bytes: usize,
}

impl AllocationPlan {
    fn required_commit_bytes(self) -> usize {
        self.required_span_storage_bytes
            .saturating_add(self.external_bytes)
    }
}

struct AllocationRoots<'a, T> {
    subsystem_roots: &'a mut dyn Trace,
    pending_value: &'a mut T,
}

impl<T: Trace> Trace for AllocationRoots<'_, T> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn crate::Tracer) {
        self.subsystem_roots.trace(tracer);
        self.pending_value.trace(tracer);
    }
}

impl Heap {
    /// Allocates and publishes one complete typed object, entering large-object policy when needed.
    pub fn try_allocate<T: Trace + 'static>(
        &mut self,
        object_type: GcType<T>,
        flags: u16,
        aux: u32,
        value: T,
        space: AllocationSpace,
    ) -> Result<GcRef<T>, HeapAllocationError> {
        self.try_allocate_accounted(AllocationRequest {
            object_type,
            flags,
            aux,
            value,
            space,
            external_bytes: 0,
        })
    }

    /// Publishes an immutable external-backed payload and charges its exact owned backing bytes.
    pub fn try_allocate_external<T: Trace + GcExternalMemory + 'static>(
        &mut self,
        object_type: GcType<T>,
        flags: u16,
        value: T,
        space: AllocationSpace,
    ) -> Result<GcRef<T>, HeapAllocationError> {
        let external_bytes = checked_external_bytes(value.external_memory_bytes())?;
        let (flags, aux) = external_header_fields(flags, external_bytes)?;
        self.try_allocate_accounted(AllocationRequest {
            object_type,
            flags,
            aux,
            value,
            space,
            external_bytes,
        })
    }

    /// Performs the one publication path after header ownership and combined quota are validated.
    fn try_allocate_accounted<T: Trace + 'static>(
        &mut self,
        request: AllocationRequest<T>,
    ) -> Result<GcRef<T>, HeapAllocationError> {
        validate_header_charge(request.flags, request.external_bytes)?;
        let plan =
            self.allocation_plan(request.object_type, request.space, request.external_bytes)?;
        self.ensure_commit_capacity(plan.required_commit_bytes())?;
        let result = if let Some(class_index) = plan.class_index {
            self.try_allocate_class(
                class_index,
                request.object_type.type_id(),
                request.flags,
                request.aux,
                request.value,
                plan.space,
            )
        } else {
            self.allocate_large(
                request.object_type.type_id(),
                request.flags,
                request.aux,
                request.value,
            )
        };
        if result.is_ok() {
            self.object_external_bytes = self
                .object_external_bytes
                .checked_add(plan.external_bytes)
                .expect("heap-limit preflight prevents external accounting overflow");
            self.trigger
                .record_allocation(plan.space, plan.allocation_bytes);
        }
        result
    }

    /// Collects at a managed allocation safepoint with complete roots, then publishes once.
    ///
    /// The pending value participates in tracing because its fields may be the only strong edges
    /// to objects that must survive the pre-allocation collection. A selected action runs at most
    /// once, so an unrecoverable resource limit cannot create a collection/retry loop.
    pub fn try_allocate_with_gc<T: Trace + 'static>(
        &mut self,
        object_type: GcType<T>,
        flags: u16,
        aux: u32,
        value: T,
        space: AllocationSpace,
        roots: &mut dyn Trace,
    ) -> Result<GcRef<T>, ManagedAllocationError> {
        self.try_allocate_accounted_with_gc(
            AllocationRequest {
                object_type,
                flags,
                aux,
                value,
                space,
                external_bytes: 0,
            },
            roots,
        )
    }

    /// Runs collection policy before publishing an immutable external-backed payload.
    pub fn try_allocate_external_with_gc<T: Trace + GcExternalMemory + 'static>(
        &mut self,
        object_type: GcType<T>,
        flags: u16,
        value: T,
        space: AllocationSpace,
        roots: &mut dyn Trace,
    ) -> Result<GcRef<T>, ManagedAllocationError> {
        let external_bytes = checked_external_bytes(value.external_memory_bytes())
            .map_err(ManagedAllocationError::Allocation)?;
        let (flags, aux) = external_header_fields(flags, external_bytes)
            .map_err(ManagedAllocationError::Allocation)?;
        self.try_allocate_accounted_with_gc(
            AllocationRequest {
                object_type,
                flags,
                aux,
                value,
                space,
                external_bytes,
            },
            roots,
        )
    }

    /// Keeps a pending payload rooted across at most one combined span/backing pressure collection.
    fn try_allocate_accounted_with_gc<T: Trace + 'static>(
        &mut self,
        mut request: AllocationRequest<T>,
        roots: &mut dyn Trace,
    ) -> Result<GcRef<T>, ManagedAllocationError> {
        validate_header_charge(request.flags, request.external_bytes)
            .map_err(ManagedAllocationError::Allocation)?;
        let plan = self
            .allocation_plan(request.object_type, request.space, request.external_bytes)
            .map_err(ManagedAllocationError::Allocation)?;
        let decision = self.trigger.decide(CollectionRequest {
            space: plan.space,
            allocation_bytes: plan.allocation_bytes,
            required_commit_bytes: plan.required_commit_bytes(),
            required_young_storage_bytes: plan.required_span_storage_bytes,
            young_storage_bytes: self.table.young_storage_bytes(),
            committed_bytes: self.committed_heap_bytes(),
            limit: self.limit,
        });
        if let Some(decision) = decision {
            self.trigger.record_attempt(decision);
            let mut allocation_roots = AllocationRoots {
                subsystem_roots: roots,
                pending_value: &mut request.value,
            };
            match decision.action {
                CollectionAction::None => {}
                CollectionAction::Minor => self
                    .collect_minor(&mut allocation_roots)
                    .map(|_| ())
                    .map_err(ManagedAllocationError::MinorCollection)?,
                CollectionAction::Major => self
                    .collect_major(&mut allocation_roots)
                    .map(|_| ())
                    .map_err(ManagedAllocationError::MajorCollection)?,
            }
        }
        request.space = plan.space;
        self.try_allocate_accounted(request)
            .map_err(ManagedAllocationError::Allocation)
    }

    /// Releases every retained empty Eden backing span, stopping at the first typed table error.
    pub fn trim_eden_pool_storage(&mut self) -> Result<usize, SpanTableError> {
        let mut released_bytes = 0_usize;
        for class_index in 0..SMALL_SIZE_CLASSES.len() {
            while let Some((pool_index, span_id)) = self.eden_pool.first_retained(class_index) {
                self.table.prepare_release(span_id)?;
                self.table.release(span_id)?;
                self.eden_pool
                    .record_trimmed(class_index, pool_index, span_id);
                self.committed_span_storage_bytes -= SPAN_SIZE_BYTES;
                released_bytes += SPAN_SIZE_BYTES;
            }
        }
        Ok(released_bytes)
    }

    /// Returns currently committed native span bytes, excluding future external-buffer accounting.
    #[must_use]
    pub const fn committed_span_storage_bytes(&self) -> usize {
        self.committed_span_storage_bytes
    }

    /// Returns separately tracked host-backed bytes charged to the same hard heap limit.
    #[must_use]
    pub const fn external_bytes(&self) -> usize {
        self.host_external_bytes
            .saturating_add(self.object_external_bytes)
    }

    /// Returns all currently charged backing bytes; side-table capacity accounting is separate.
    #[must_use]
    pub const fn committed_heap_bytes(&self) -> usize {
        self.committed_span_storage_bytes
            .saturating_add(self.external_bytes())
    }

    /// Charges an external string/buffer backing before the host publishes it to JavaScript.
    pub fn try_charge_external(&mut self, bytes: usize) -> Result<(), HeapAllocationError> {
        let committed = self.committed_heap_bytes();
        let total = committed.saturating_add(bytes);
        if total > self.limit.max_heap_bytes() {
            return Err(HeapAllocationError::HeapLimitExceeded {
                limit: self.limit.max_heap_bytes(),
                committed,
                requested: bytes,
            });
        }
        self.host_external_bytes = self.host_external_bytes.saturating_add(bytes);
        Ok(())
    }

    /// Releases a previously charged external backing allocation.
    pub fn release_external(&mut self, bytes: usize) -> bool {
        let Some(remaining) = self.host_external_bytes.checked_sub(bytes) else {
            return false;
        };
        self.host_external_bytes = remaining;
        true
    }

    /// Computes effective generation, charged object bytes, and storage growth without mutation.
    fn allocation_plan<T: Trace + 'static>(
        &self,
        object_type: GcType<T>,
        requested_space: AllocationSpace,
        external_bytes: usize,
    ) -> Result<AllocationPlan, HeapAllocationError> {
        if !self.types.matches(object_type) {
            return Err(HeapAllocationError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        let space = if object_type.descriptor().allocation_policy() == GcAllocationPolicy::OldOnly {
            AllocationSpace::Old
        } else {
            requested_space
        };
        if let Ok(layout) = SmallObjectLayout::for_type::<T>()
            && let Some(class_index) = size_class_index(layout.slot_size())
        {
            let active = match space {
                AllocationSpace::Young => self.active_eden[class_index],
                AllocationSpace::Old => self.active_old[class_index],
            };
            let required_storage_bytes = if active
                .is_some_and(|span_id| self.table.can_allocate_in_span(span_id))
                || (space == AllocationSpace::Young && self.eden_pool.has_retained(class_index))
            {
                0
            } else {
                SPAN_SIZE_BYTES
            };
            return Ok(AllocationPlan {
                space,
                class_index: Some(class_index),
                allocation_bytes: usize::from(SMALL_SIZE_CLASSES[class_index])
                    .saturating_add(external_bytes),
                required_span_storage_bytes: required_storage_bytes,
                external_bytes,
            });
        }
        let required_storage_bytes = large_storage_bytes::<T>()?;
        Ok(AllocationPlan {
            space: AllocationSpace::Old,
            class_index: None,
            allocation_bytes: required_storage_bytes.saturating_add(external_bytes),
            required_span_storage_bytes: required_storage_bytes,
            external_bytes,
        })
    }

    /// Rejects a combined span/backing publication before either ownership domain mutates.
    fn ensure_commit_capacity(&self, requested: usize) -> Result<(), HeapAllocationError> {
        let committed = self.committed_heap_bytes();
        if committed.saturating_add(requested) > self.limit.max_heap_bytes() {
            return Err(HeapAllocationError::HeapLimitExceeded {
                limit: self.limit.max_heap_bytes(),
                committed,
                requested,
            });
        }
        Ok(())
    }

    /// Updates committed storage after the collector has dropped a large payload and releases its range.
    pub fn reclaim_large_after_drop(
        &mut self,
        reference: RawHeapRef,
    ) -> Result<LargeReclaim, SpanTableError> {
        let reclaimed = self.table.reclaim_large_after_drop(reference)?;
        self.committed_span_storage_bytes -= reclaimed.storage_bytes();
        Ok(reclaimed)
    }

    /// Removes cached allocator IDs whose backing spans were released by full sweep.
    pub(super) fn clear_released_active_spans(&mut self) {
        for active in self.active_eden.iter_mut().chain(&mut self.active_old) {
            if active.is_some_and(|span_id| self.table.sweep_target(span_id).is_none()) {
                *active = None;
            }
        }
    }

    /// Drops stale Eden caches and adopts promoted Old spans with reusable holes by size class.
    pub(super) fn repair_active_spans_after_minor(
        &mut self,
        promoted_active_old: [Option<SpanId>; SMALL_SIZE_CLASSES.len()],
    ) {
        self.active_eden.fill(None);
        for (active, promoted) in self.active_old.iter_mut().zip(promoted_active_old) {
            if active.is_some_and(|span_id| !self.table.can_allocate_in_span(span_id)) {
                *active = None;
            }
            if active.is_none() {
                *active = promoted;
            }
        }
    }

    #[inline(always)]
    fn try_allocate_class<T: Trace>(
        &mut self,
        class_index: usize,
        type_id: GcTypeId,
        flags: u16,
        aux: u32,
        value: T,
        space: AllocationSpace,
    ) -> Result<GcRef<T>, HeapAllocationError> {
        let active = match space {
            AllocationSpace::Young => &mut self.active_eden[class_index],
            AllocationSpace::Old => &mut self.active_old[class_index],
        };
        if let Some(span_id) = *active {
            if self.table.can_allocate_in_span(span_id) {
                return self
                    .table
                    .try_allocate_in_span(span_id, type_id, flags, aux, value)
                    .map_err(HeapAllocationError::SpanAllocation);
            }
            *active = None;
        }

        self.allocate_slow(class_index, type_id, flags, aux, value, space)
    }

    /// Enforces the resource cap before table/storage growth, then installs one new active span.
    fn allocate_slow<T: Trace>(
        &mut self,
        class_index: usize,
        type_id: GcTypeId,
        flags: u16,
        aux: u32,
        value: T,
        space: AllocationSpace,
    ) -> Result<GcRef<T>, HeapAllocationError> {
        if space == AllocationSpace::Young
            && let Some(span_id) = self.eden_pool.take_for_reuse(class_index)
        {
            self.table.activate_pooled_eden(span_id);
            self.active_eden[class_index] = Some(span_id);
            return self
                .table
                .try_allocate_in_span(span_id, type_id, flags, aux, value)
                .map_err(HeapAllocationError::SpanAllocation);
        }
        let committed = self.committed_heap_bytes();
        let total = committed.saturating_add(SPAN_SIZE_BYTES);
        if total > self.limit.max_heap_bytes() {
            return Err(HeapAllocationError::HeapLimitExceeded {
                limit: self.limit.max_heap_bytes(),
                committed,
                requested: SPAN_SIZE_BYTES,
            });
        }
        let size_class = crate::SizeClass::new(SMALL_SIZE_CLASSES[class_index])
            .expect("tuning size classes satisfy representation invariants");
        let span_space = match space {
            AllocationSpace::Young => SpanSpace::Eden,
            AllocationSpace::Old => SpanSpace::Old,
        };
        let span_id = self
            .table
            .try_allocate_small(size_class, span_space)
            .map_err(HeapAllocationError::SpanTable)?;
        self.committed_span_storage_bytes += SPAN_SIZE_BYTES;
        match space {
            AllocationSpace::Young => self.active_eden[class_index] = Some(span_id),
            AllocationSpace::Old => self.active_old[class_index] = Some(span_id),
        }
        self.table
            .try_allocate_in_span(span_id, type_id, flags, aux, value)
            .map_err(HeapAllocationError::SpanAllocation)
    }

    /// Allocates every large, pinned-size payload directly in old space after one limit check.
    fn allocate_large<T: Trace>(
        &mut self,
        type_id: GcTypeId,
        flags: u16,
        aux: u32,
        value: T,
    ) -> Result<GcRef<T>, HeapAllocationError> {
        let requested = large_storage_bytes::<T>()?;
        let current = self.committed_heap_bytes();
        let committed = current.saturating_add(requested);
        if committed > self.limit.max_heap_bytes() {
            return Err(HeapAllocationError::HeapLimitExceeded {
                limit: self.limit.max_heap_bytes(),
                committed: current,
                requested,
            });
        }
        let (reference, actual_bytes) = self
            .table
            .try_allocate_large(type_id, flags, aux, value)
            .map_err(HeapAllocationError::LargeAllocation)?;
        debug_assert_eq!(actual_bytes, requested);
        self.committed_span_storage_bytes += requested;
        Ok(reference)
    }
}

fn checked_external_bytes(bytes: usize) -> Result<usize, HeapAllocationError> {
    if bytes > u32::MAX as usize {
        return Err(HeapAllocationError::ExternalBytesTooLarge {
            bytes,
            maximum: u32::MAX as usize,
        });
    }
    Ok(bytes)
}

fn external_header_fields(
    flags: u16,
    external_bytes: usize,
) -> Result<(u16, u32), HeapAllocationError> {
    if flags & GC_HEADER_EXTERNAL_BYTES_FLAG != 0 {
        return Err(HeapAllocationError::ReservedHeaderFlag { flags });
    }
    if external_bytes == 0 {
        return Ok((flags, 0));
    }
    Ok((flags | GC_HEADER_EXTERNAL_BYTES_FLAG, external_bytes as u32))
}

fn validate_header_charge(flags: u16, external_bytes: usize) -> Result<(), HeapAllocationError> {
    let has_external_charge = flags & GC_HEADER_EXTERNAL_BYTES_FLAG != 0;
    if has_external_charge != (external_bytes != 0) {
        return Err(HeapAllocationError::ReservedHeaderFlag { flags });
    }
    Ok(())
}

/// Rounds a large object's header/payload layout to its contiguous logical span charge.
fn large_storage_bytes<T>() -> Result<usize, HeapAllocationError> {
    let layout = ObjectLayout::for_type::<T>().map_err(|_| {
        HeapAllocationError::LargeAllocation(LargeAllocationError::AddressSpaceExhausted)
    })?;
    let logical_bytes = crate::MINIMUM_SLOT_SIZE_BYTES
        .checked_add(layout.allocation_size())
        .ok_or(HeapAllocationError::LargeAllocation(
            LargeAllocationError::AddressSpaceExhausted,
        ))?;
    logical_bytes
        .div_ceil(SPAN_SIZE_BYTES)
        .checked_mul(SPAN_SIZE_BYTES)
        .ok_or(HeapAllocationError::LargeAllocation(
            LargeAllocationError::AddressSpaceExhausted,
        ))
}

#[inline(always)]
fn size_class_index(required: u16) -> Option<usize> {
    let index = SMALL_SIZE_CLASSES.partition_point(|&class| class < required);
    (index < SMALL_SIZE_CLASSES.len()).then_some(index)
}
