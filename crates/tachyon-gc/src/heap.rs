//! Small-object allocation policy over fixed logical spans.

use crate::{
    BarrierVerificationError, CollectionEpoch, FinalizationQueueStats, GcAllocationPolicy,
    GcHeader, GcRef, GcType, GcTypeId, GrayQueueStats, HeapReferenceError, KeptObjectStats,
    LargeAllocationError, LargeReclaim, MAX_LOGICAL_OBJECT_COUNT, MAX_LOGICAL_SPANS, MarkError,
    MarkStats, MinorSweepStats, ObjectLayout, RawHeapRef, SPAN_SIZE_BYTES, SmallAllocationError,
    SmallObjectLayout, SpanId, SpanSpace, SpanTable, SpanTableError, SweepError, SweepStats,
    SweepWorklistStats, TemporaryRootStats, Trace, TypeRegistry, WeakOwnerStats, YoungMarkStats,
    finalization::PendingFinalizations,
    gray::GrayQueue,
    mark::{mark_strong_roots, mark_young_roots},
    persistent::{PersistentRootError, PersistentRootId, PersistentRootStats, PersistentRoots},
    roots::{KeptObjectError, KeptObjects, RootComposition, TemporaryRoots},
    scope::{NoGcBorrowError, RootError, RunningScope},
    sweep::{SweepWorklist, sweep_full, sweep_young},
    trigger::{CollectionAction, GcTrigger},
    tuning::SMALL_SIZE_CLASSES,
    weak::WeakOwners,
};

/// Whether an object enters the young bump path or is allocated directly in old space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationSpace {
    Young,
    Old,
}

/// A host-configured hard cap shared by native spans and external backing stores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeapLimit {
    max_heap_bytes: usize,
}

impl HeapLimit {
    #[must_use]
    pub const fn new(max_heap_bytes: usize) -> Self {
        Self { max_heap_bytes }
    }

    #[must_use]
    pub const fn max_heap_bytes(self) -> usize {
        self.max_heap_bytes
    }
}

/// A structured small-heap failure; no branch falls back to infallible allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeapAllocationError {
    UnregisteredOrMismatchedType {
        type_id: GcTypeId,
    },
    HeapLimitExceeded {
        limit: usize,
        committed: usize,
        requested: usize,
    },
    SpanTable(SpanTableError),
    SpanAllocation(SmallAllocationError),
    LargeAllocation(LargeAllocationError),
}

/// A full-major collection fails before sweep or leaves any partial sweep exactly accounted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MajorCollectionError {
    Mark(MarkError),
    Sweep(SweepError),
}

/// A minor collection fails during young marking or a partially accounted young sweep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MinorCollectionError {
    Barrier(BarrierVerificationError),
    Mark(MarkError),
    Sweep(SweepError),
}

/// A managed allocation reports collection and publication failures without erasing their phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedAllocationError {
    Allocation(HeapAllocationError),
    MinorCollection(MinorCollectionError),
    MajorCollection(MajorCollectionError),
}

/// Combined fixed-point and sweep evidence for one stop-the-world full major collection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MajorCollectionStats {
    pub mark: MarkStats,
    pub sweep: SweepStats,
}

/// Combined remembered marking and non-moving cohort sweep evidence for one minor collection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MinorCollectionStats {
    pub mark: YoungMarkStats,
    pub sweep: MinorSweepStats,
}

/// A single-mutator heap with fixed small-size-class slots and direct-old large ranges.
pub struct Heap {
    types: TypeRegistry,
    table: SpanTable,
    active_eden: [Option<SpanId>; SMALL_SIZE_CLASSES.len()],
    active_old: [Option<SpanId>; SMALL_SIZE_CLASSES.len()],
    limit: HeapLimit,
    committed_span_storage_bytes: usize,
    external_bytes: usize,
    collection_epoch: CollectionEpoch,
    gray: GrayQueue,
    sweep_worklist: SweepWorklist,
    temporary_roots: TemporaryRoots,
    persistent_roots: PersistentRoots,
    weak_owners: WeakOwners,
    kept_objects: KeptObjects,
    pending_finalizations: PendingFinalizations,
    trigger: GcTrigger,
}

#[derive(Clone, Copy)]
struct AllocationPlan {
    space: AllocationSpace,
    class_index: Option<usize>,
    allocation_bytes: usize,
    required_storage_bytes: usize,
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
    /// Creates an empty heap without allocating a span or active-size-class side container.
    #[must_use]
    pub const fn new(limit: HeapLimit, types: TypeRegistry) -> Self {
        Self::with_trigger_config(limit, types, crate::trigger::GcTriggerConfig::DEFAULT)
    }

    /// Creates an empty heap with validated host collection policy and no eager backing storage.
    #[must_use]
    pub const fn with_trigger_config(
        limit: HeapLimit,
        types: TypeRegistry,
        trigger_config: crate::GcTriggerConfig,
    ) -> Self {
        let limit_spans = limit.max_heap_bytes().div_ceil(SPAN_SIZE_BYTES);
        let max_sweep_entries = if limit_spans < MAX_LOGICAL_SPANS {
            limit_spans
        } else {
            MAX_LOGICAL_SPANS
        };
        let limit_objects = limit.max_heap_bytes() / crate::MINIMUM_SLOT_SIZE_BYTES;
        let max_reference_entries = if limit_objects < MAX_LOGICAL_OBJECT_COUNT {
            limit_objects
        } else {
            MAX_LOGICAL_OBJECT_COUNT
        };
        Self {
            types,
            table: SpanTable::new(),
            active_eden: [None; SMALL_SIZE_CLASSES.len()],
            active_old: [None; SMALL_SIZE_CLASSES.len()],
            limit,
            committed_span_storage_bytes: 0,
            external_bytes: 0,
            collection_epoch: CollectionEpoch::INITIAL,
            gray: GrayQueue::new(max_reference_entries),
            sweep_worklist: SweepWorklist::new(max_sweep_entries),
            temporary_roots: TemporaryRoots::new(max_reference_entries),
            persistent_roots: PersistentRoots::new(max_reference_entries),
            weak_owners: WeakOwners::new(max_reference_entries),
            kept_objects: KeptObjects::new(max_reference_entries),
            pending_finalizations: PendingFinalizations::new(max_reference_entries),
            trigger: GcTrigger::new(trigger_config),
        }
    }

    /// Allocates and publishes one complete typed object, entering large-object policy when needed.
    pub fn try_allocate<T: Trace + 'static>(
        &mut self,
        object_type: GcType<T>,
        flags: u16,
        aux: u32,
        value: T,
        space: AllocationSpace,
    ) -> Result<GcRef<T>, HeapAllocationError> {
        let plan = self.allocation_plan(object_type, space)?;
        let result = if let Some(class_index) = plan.class_index {
            self.try_allocate_class(
                class_index,
                object_type.type_id(),
                flags,
                aux,
                value,
                plan.space,
            )
        } else {
            self.allocate_large(object_type.type_id(), flags, aux, value)
        };
        if result.is_ok() {
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
        mut value: T,
        space: AllocationSpace,
        roots: &mut dyn Trace,
    ) -> Result<GcRef<T>, ManagedAllocationError> {
        let plan = self
            .allocation_plan(object_type, space)
            .map_err(ManagedAllocationError::Allocation)?;
        let decision = self.trigger.decide(
            plan.space,
            plan.allocation_bytes,
            plan.required_storage_bytes,
            self.committed_heap_bytes(),
            self.limit,
        );
        if let Some(decision) = decision {
            self.trigger.record_attempt(decision);
            let mut allocation_roots = AllocationRoots {
                subsystem_roots: roots,
                pending_value: &mut value,
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
        self.try_allocate(object_type, flags, aux, value, plan.space)
            .map_err(ManagedAllocationError::Allocation)
    }

    #[must_use]
    pub const fn trigger_config(&self) -> crate::GcTriggerConfig {
        self.trigger.config()
    }

    #[must_use]
    pub const fn forced_collection_mode(&self) -> crate::ForcedCollectionMode {
        self.trigger.forced_mode()
    }

    pub fn set_forced_collection_mode(&mut self, mode: crate::ForcedCollectionMode) {
        self.trigger.set_forced_mode(mode);
    }

    /// Coalesces host pressure notifications into one full major at the next managed allocation.
    pub fn request_memory_pressure_collection(&mut self) {
        self.trigger.request_memory_pressure();
    }

    #[must_use]
    pub const fn trigger_stats(&self) -> crate::GcTriggerStats {
        self.trigger.stats()
    }

    /// Returns the logical table for collection and exact verifier operations.
    #[must_use]
    pub const fn span_table(&self) -> &SpanTable {
        &self.table
    }

    /// Returns the immutable descriptor table used to validate header IDs and invoke callbacks.
    #[must_use]
    pub const fn type_registry(&self) -> &TypeRegistry {
        &self.types
    }

    /// Returns mutable table ownership only to isolate-local collection code.
    #[must_use]
    pub const fn span_table_mut(&mut self) -> &mut SpanTable {
        &mut self.table
    }

    /// Returns currently committed native span bytes, excluding future external-buffer accounting.
    #[must_use]
    pub const fn committed_span_storage_bytes(&self) -> usize {
        self.committed_span_storage_bytes
    }

    /// Returns separately tracked host-backed bytes charged to the same hard heap limit.
    #[must_use]
    pub const fn external_bytes(&self) -> usize {
        self.external_bytes
    }

    /// Returns all currently charged backing bytes; side-table capacity accounting is separate.
    #[must_use]
    pub const fn committed_heap_bytes(&self) -> usize {
        self.committed_span_storage_bytes
            .saturating_add(self.external_bytes)
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
        self.external_bytes = self.external_bytes.saturating_add(bytes);
        Ok(())
    }

    /// Releases a previously charged external backing allocation.
    pub fn release_external(&mut self, bytes: usize) -> bool {
        let Some(remaining) = self.external_bytes.checked_sub(bytes) else {
            return false;
        };
        self.external_bytes = remaining;
        true
    }

    /// Verifies side metadata and rejects non-zero header IDs absent from this heap's registry.
    pub fn verify_reference(
        &self,
        reference: RawHeapRef,
        expected_type: Option<GcTypeId>,
    ) -> Result<GcHeader, HeapReferenceError> {
        let header = self.table.verify_reference(reference, expected_type)?;
        let type_id = header
            .type_id()
            .expect("table verification already rejected a zero type ID");
        if self.types.descriptor(type_id).is_none() {
            return Err(HeapReferenceError::UnregisteredTypeId { reference, type_id });
        }
        Ok(header)
    }

    /// Starts a fresh epoch and reaches the exact strong-root fixed point iteratively.
    pub fn mark_strong(&mut self, roots: &mut dyn Trace) -> Result<MarkStats, MarkError> {
        self.collection_epoch = self.table.advance_collection_epoch(self.collection_epoch);
        let mut roots = RootComposition::new(
            roots,
            &mut self.temporary_roots,
            &mut self.persistent_roots,
            &mut self.kept_objects,
        );
        mark_strong_roots(
            &mut self.table,
            &self.types,
            &mut self.gray,
            &mut self.weak_owners,
            &mut self.pending_finalizations,
            self.collection_epoch,
            &mut roots,
        )
    }

    /// Marks young reachability from exact roots and the bounded remembered set without sweeping.
    pub fn mark_young(&mut self, roots: &mut dyn Trace) -> Result<YoungMarkStats, MarkError> {
        self.collection_epoch = self.table.advance_collection_epoch(self.collection_epoch);
        let mut roots = RootComposition::new(
            roots,
            &mut self.temporary_roots,
            &mut self.persistent_roots,
            &mut self.kept_objects,
        );
        mark_young_roots(
            &mut self.table,
            &self.types,
            &mut self.gray,
            &mut self.weak_owners,
            &mut self.pending_finalizations,
            self.collection_epoch,
            &mut roots,
        )
    }

    /// Applies the Phase 1B post-write barrier after a heap field stores a target reference.
    #[inline(always)]
    pub fn write_barrier(
        &mut self,
        source: RawHeapRef,
        target: RawHeapRef,
    ) -> Result<bool, HeapReferenceError> {
        self.table.remember_old_to_young(source, target)
    }

    /// Creates an unforgeable local-handle lifetime and always rolls back its root checkpoint.
    ///
    /// Returning a `Local` from the callback is rejected because `R` must be valid for every fresh
    /// scope lifetime:
    ///
    /// ```compile_fail
    /// use tachyon_gc::{GcRef, Heap, HeapLimit, RawHeapRef, TypeRegistry};
    /// let mut heap = Heap::new(HeapLimit::new(64 * 1024), TypeRegistry::new());
    /// // SAFETY: compile-fail fixture only needs a typed token; no dereference is performed.
    /// let reference = unsafe { GcRef::<()>::from_raw_unchecked(RawHeapRef::new(16).unwrap()) };
    /// let escaped = heap.with_running_scope(|scope| scope.root(reference));
    /// ```
    pub fn with_running_scope<R>(
        &mut self,
        callback: impl for<'scope> FnOnce(&mut RunningScope<'_, 'scope>) -> R,
    ) -> R {
        let checkpoint = self.temporary_roots.len();
        let mut scope = RunningScope::new(self, checkpoint);
        callback(&mut scope)
    }

    /// Returns retained gray high-water evidence for tuning and quota tests.
    #[must_use]
    pub fn gray_queue_stats(&self) -> GrayQueueStats {
        self.gray.stats()
    }

    /// Marks exact roots and then sweeps every span owner without a per-object work vector.
    pub fn collect_major(
        &mut self,
        roots: &mut dyn Trace,
    ) -> Result<MajorCollectionStats, MajorCollectionError> {
        let mark = self
            .mark_strong(roots)
            .map_err(MajorCollectionError::Mark)?;
        let mut sweep = SweepStats::default();
        let result = sweep_full(
            &mut self.table,
            &self.types,
            &mut self.sweep_worklist,
            self.collection_epoch,
            &mut sweep,
        );
        self.committed_span_storage_bytes = self
            .committed_span_storage_bytes
            .checked_sub(sweep.released_storage_bytes)
            .expect("sweep cannot release more storage than the heap committed");
        sweep.external_bytes = self.external_bytes;
        self.clear_released_active_spans();
        result.map_err(MajorCollectionError::Sweep)?;
        self.trigger
            .record_collection_success(CollectionAction::Major);
        Ok(MajorCollectionStats { mark, sweep })
    }

    /// Marks and sweeps only young cohorts, preserving every surviving logical/native address.
    pub fn collect_minor(
        &mut self,
        roots: &mut dyn Trace,
    ) -> Result<MinorCollectionStats, MinorCollectionError> {
        #[cfg(any(test, feature = "barrier-verifier"))]
        self.verify_generational_barriers()
            .map_err(MinorCollectionError::Barrier)?;
        let mark = self.mark_young(roots).map_err(MinorCollectionError::Mark)?;
        let mut sweep = MinorSweepStats::default();
        let mut promoted_active_old = [None; SMALL_SIZE_CLASSES.len()];
        let result = sweep_young(
            &mut self.table,
            &self.types,
            self.collection_epoch,
            &mut promoted_active_old,
            &mut sweep,
        );
        self.committed_span_storage_bytes = self
            .committed_span_storage_bytes
            .checked_sub(sweep.sweep.released_storage_bytes)
            .expect("minor sweep cannot release more storage than the heap committed");
        sweep.sweep.external_bytes = self.external_bytes;
        self.repair_active_spans_after_minor(promoted_active_old);
        result.map_err(MinorCollectionError::Sweep)?;
        self.trigger
            .record_collection_success(CollectionAction::Minor);
        Ok(MinorCollectionStats { mark, sweep })
    }

    /// Performs a diagnostic full-Old-heap scan without changing GC liveness or weak state.
    pub fn verify_generational_barriers(
        &mut self,
    ) -> Result<crate::BarrierVerificationStats, crate::BarrierVerificationError> {
        crate::barrier::verify_generational_barriers(&mut self.table, &self.types)
    }

    /// Returns retained span-worklist high water after successful or failed collection attempts.
    #[must_use]
    pub fn sweep_worklist_stats(&self) -> SweepWorklistStats {
        self.sweep_worklist.stats()
    }

    /// Returns temporary-root capacity evidence without exposing stack mutation.
    #[must_use]
    pub fn temporary_root_stats(&self) -> TemporaryRootStats {
        self.temporary_roots.stats()
    }

    /// Returns isolate-owned persistent root slot usage and retained capacity.
    #[must_use]
    pub fn persistent_root_stats(&self) -> PersistentRootStats {
        self.persistent_roots.stats()
    }

    /// Returns weak-owner high-water evidence retained across collection phases.
    #[must_use]
    pub fn weak_owner_stats(&self) -> WeakOwnerStats {
        self.weak_owners.stats()
    }

    /// Returns job-scoped kept-root high-water evidence.
    #[must_use]
    pub fn kept_object_stats(&self) -> KeptObjectStats {
        self.kept_objects.stats()
    }

    /// Returns pending cleanup records and retained queue capacity.
    #[must_use]
    pub fn finalization_queue_stats(&self) -> FinalizationQueueStats {
        self.pending_finalizations.stats()
    }

    /// Transfers one cleanup command to the isolate safepoint scheduler.
    ///
    /// The record leaves the queue's root set; a scheduler must root its heap fields before any
    /// allocation or JavaScript cleanup callback can run.
    pub fn pop_pending_finalization(&mut self) -> Option<crate::PendingFinalization> {
        self.pending_finalizations.pop()
    }

    /// Validates and retains one WeakRef dereference target until the host ends the current job.
    pub(crate) fn keep_alive(&mut self, reference: RawHeapRef) -> Result<bool, KeptObjectError> {
        self.verify_reference(reference, None)
            .map_err(KeptObjectError::InvalidReference)?;
        self.kept_objects.try_insert(reference)
    }

    /// Clears AddToKeptObjects roots only at an explicit ECMAScript job boundary.
    pub fn clear_kept_objects_at_job_boundary(&mut self) {
        self.kept_objects.clear();
    }

    pub(crate) const fn temporary_root_count(&self) -> usize {
        self.temporary_roots.len()
    }

    pub(crate) fn truncate_temporary_roots(&mut self, checkpoint: usize) {
        self.temporary_roots.truncate(checkpoint);
    }

    pub(crate) fn try_push_temporary_root(
        &mut self,
        reference: RawHeapRef,
    ) -> Result<(), RootError> {
        self.verify_reference(reference, None)
            .map_err(RootError::InvalidReference)?;
        self.temporary_roots
            .try_push(reference)
            .map_err(RootError::Capacity)
    }

    /// Revalidates heap registry, header type, layout, and liveness before a shared payload borrow.
    pub(crate) fn checked_payload_shared<T: Trace + 'static>(
        &self,
        reference: GcRef<T>,
        object_type: GcType<T>,
    ) -> Result<*const T, NoGcBorrowError> {
        if !self.types.matches(object_type) {
            return Err(NoGcBorrowError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        let descriptor = self
            .types
            .descriptor(object_type.type_id())
            .expect("matching type tokens have immutable descriptors");
        self.table
            .payload_address_shared(reference.raw(), descriptor)
            .map(|payload| payload.cast::<T>())
            .map_err(NoGcBorrowError::InvalidReference)
    }

    pub(crate) fn create_persistent_root<T: Trace + 'static>(
        &mut self,
        reference: GcRef<T>,
        object_type: GcType<T>,
    ) -> Result<PersistentRootId<T>, PersistentRootError> {
        self.validate_persistent_reference(reference, object_type)?;
        self.persistent_roots
            .try_insert(reference.raw(), object_type.type_id())
    }

    pub(crate) fn clone_persistent_root<T: Trace + 'static>(
        &mut self,
        id: PersistentRootId<T>,
        object_type: GcType<T>,
    ) -> Result<PersistentRootId<T>, PersistentRootError> {
        if !self.types.matches(object_type) {
            return Err(PersistentRootError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        self.persistent_roots.try_clone(id, object_type.type_id())
    }

    pub(crate) fn resolve_persistent_root<T: Trace + 'static>(
        &mut self,
        id: PersistentRootId<T>,
        object_type: GcType<T>,
    ) -> Result<GcRef<T>, PersistentRootError> {
        if !self.types.matches(object_type) {
            return Err(PersistentRootError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        let reference = self.persistent_roots.resolve(id, object_type.type_id())?;
        let reference = GcRef::from_raw(reference);
        self.validate_persistent_reference(reference, object_type)?;
        Ok(reference)
    }

    pub(crate) fn release_persistent_root<T: Trace + 'static>(
        &mut self,
        id: PersistentRootId<T>,
        object_type: GcType<T>,
    ) -> Result<(), PersistentRootError> {
        if !self.types.matches(object_type) {
            return Err(PersistentRootError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        self.persistent_roots.release(id, object_type.type_id())
    }

    fn validate_persistent_reference<T: Trace + 'static>(
        &self,
        reference: GcRef<T>,
        object_type: GcType<T>,
    ) -> Result<(), PersistentRootError> {
        if !self.types.matches(object_type) {
            return Err(PersistentRootError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        self.verify_reference(reference.raw(), Some(object_type.type_id()))
            .map(|_| ())
            .map_err(PersistentRootError::InvalidReference)
    }

    /// Computes effective generation, charged object bytes, and storage growth without mutation.
    fn allocation_plan<T: Trace + 'static>(
        &self,
        object_type: GcType<T>,
        requested_space: AllocationSpace,
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
            let required_storage_bytes =
                if active.is_some_and(|span_id| self.table.can_allocate_in_span(span_id)) {
                    0
                } else {
                    SPAN_SIZE_BYTES
                };
            return Ok(AllocationPlan {
                space,
                class_index: Some(class_index),
                allocation_bytes: usize::from(SMALL_SIZE_CLASSES[class_index]),
                required_storage_bytes,
            });
        }
        let required_storage_bytes = large_storage_bytes::<T>()?;
        Ok(AllocationPlan {
            space: AllocationSpace::Old,
            class_index: None,
            allocation_bytes: required_storage_bytes,
            required_storage_bytes,
        })
    }

    /// Revalidates heap registry, header type, layout, and liveness before an exclusive borrow.
    pub(crate) fn checked_payload_mut<T: Trace + 'static>(
        &mut self,
        reference: GcRef<T>,
        object_type: GcType<T>,
    ) -> Result<core::ptr::NonNull<T>, NoGcBorrowError> {
        if !self.types.matches(object_type) {
            return Err(NoGcBorrowError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        let descriptor = self
            .types
            .descriptor(object_type.type_id())
            .expect("matching type tokens have immutable descriptors");
        self.table
            .payload_address(reference.raw(), descriptor)
            .map(core::ptr::NonNull::cast)
            .map_err(NoGcBorrowError::InvalidReference)
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
    fn clear_released_active_spans(&mut self) {
        for active in self.active_eden.iter_mut().chain(&mut self.active_old) {
            if active.is_some_and(|span_id| self.table.sweep_target(span_id).is_none()) {
                *active = None;
            }
        }
    }

    /// Drops stale Eden caches and adopts promoted Old spans with reusable holes by size class.
    fn repair_active_spans_after_minor(
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

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::{AllocationSpace, Heap, HeapAllocationError, HeapLimit};
    use crate::{
        BarrierVerificationError, CardBitmap, Ephemeron, FinalizationRegistration,
        ForcedCollectionMode, GcRef, GcTriggerConfig, HeapReferenceError, ManagedAllocationError,
        MinorCollectionError, RawHeapRef, SPAN_SIZE_BYTES, SpanSpace, Trace, Tracer,
        TypeRegistrationError, TypeRegistry, WeakGcRef,
    };
    use tachyon_value::Value;

    struct OtherPayload;

    #[derive(Debug, Eq, PartialEq)]
    struct LargePayload {
        _bytes: [u8; 70_000],
    }

    struct ChainNode {
        next: Option<GcRef<ChainNode>>,
    }

    struct LargeEdgeNode {
        _bytes: [u8; 70_000],
        next: Option<GcRef<ChainNode>>,
    }

    struct Leaf;

    struct Fanout {
        edges: [Option<GcRef<Leaf>>; 300],
    }

    struct WeakHolder {
        target: WeakGcRef<ChainNode>,
    }

    struct EphemeronHolder {
        entry: Ephemeron<ChainNode>,
    }

    struct FinalizationHolder {
        registration: FinalizationRegistration<ChainNode>,
    }

    struct PinnedPayload;

    struct DropNode {
        next: Option<GcRef<DropNode>>,
        drops: Arc<AtomicUsize>,
    }

    struct DropLarge {
        _bytes: [u8; 70_000],
        drops: Arc<AtomicUsize>,
    }

    struct PanicOnDrop {
        drops: Arc<AtomicUsize>,
    }

    struct LargePanicOnDrop {
        _bytes: [u8; 70_000],
        drops: Arc<AtomicUsize>,
    }

    struct StressRoots {
        stable: Vec<Value>,
        nodes: Vec<GcRef<ChainNode>>,
    }

    impl StressRoots {
        fn new() -> Self {
            Self {
                stable: Vec::with_capacity(8),
                nodes: Vec::with_capacity(16),
            }
        }
    }

    impl Trace for StressRoots {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.stable.trace(tracer);
            self.nodes.trace(tracer);
        }
    }

    struct DeterministicRng(u64);

    impl DeterministicRng {
        #[inline]
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    impl Trace for OtherPayload {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for LargePayload {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for ChainNode {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.next.trace(tracer);
        }
    }

    impl Trace for LargeEdgeNode {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.next.trace(tracer);
        }
    }

    impl Trace for Leaf {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for Fanout {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.edges.trace(tracer);
        }
    }

    impl Trace for WeakHolder {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.target.trace(tracer);
        }
    }

    impl Trace for EphemeronHolder {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.entry.trace(tracer);
        }
    }

    impl Trace for FinalizationHolder {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.registration.trace(tracer);
        }
    }

    impl Trace for PinnedPayload {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for DropNode {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.next.trace(tracer);
        }
    }

    impl Drop for DropNode {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Trace for DropLarge {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Drop for DropLarge {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Trace for PanicOnDrop {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
            panic!("intentional destructor unwind");
        }
    }

    impl Trace for LargePanicOnDrop {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Drop for LargePanicOnDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
            panic!("intentional large destructor unwind");
        }
    }

    #[test]
    fn first_allocation_uses_slow_path_then_reuses_the_active_eden_span() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<Value>("Value").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let first = heap
            .try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(1),
                AllocationSpace::Young,
            )
            .unwrap();
        let second = heap
            .try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(2),
                AllocationSpace::Young,
            )
            .unwrap();

        assert_eq!(first.raw().span_id(), second.raw().span_id());
        assert_ne!(first.raw(), second.raw());
        assert_eq!(heap.committed_span_storage_bytes(), SPAN_SIZE_BYTES);
        assert_eq!(heap.span_table().live_spans(), 1);
        assert_eq!(
            heap.verify_reference(first.raw(), Some(object_type.type_id()))
                .unwrap()
                .type_id(),
            Some(object_type.type_id())
        );
    }

    #[test]
    /// Pending object fields join complete roots before a forced pre-allocation minor collection.
    fn forced_minor_traces_pending_value_and_reclaims_other_young_objects() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
        let mut heap =
            Heap::with_trigger_config(HeapLimit::new(2 * SPAN_SIZE_BYTES), types, config);
        let target = heap
            .try_allocate(
                object_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let dead = heap
            .try_allocate(
                object_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        heap.set_forced_collection_mode(ForcedCollectionMode::Minor);
        let mut no_roots = Vec::<Value>::new();

        let parent = heap
            .try_allocate_with_gc(
                object_type,
                0,
                0,
                ChainNode { next: Some(target) },
                AllocationSpace::Young,
                &mut no_roots,
            )
            .unwrap();

        assert!(heap.verify_reference(target.raw(), None).is_ok());
        assert!(heap.verify_reference(parent.raw(), None).is_ok());
        assert_eq!(
            heap.verify_reference(dead.raw(), None),
            Err(HeapReferenceError::UnallocatedSlot(dead.raw()))
        );
        let stats = heap.trigger_stats();
        assert_eq!(stats.minor_attempts, 1);
        assert_eq!(stats.minor_successes, 1);
    }

    #[test]
    /// Forced major runs at every managed allocation point and preserves explicit subsystem roots.
    fn forced_major_runs_per_allocation_and_traces_explicit_roots() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
        let mut heap =
            Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
        let mut root = heap
            .try_allocate(
                object_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        heap.set_forced_collection_mode(ForcedCollectionMode::Major);

        for _ in 0..2 {
            heap.try_allocate_with_gc(
                object_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
                &mut root,
            )
            .unwrap();
        }

        assert!(heap.verify_reference(root.raw(), None).is_ok());
        let stats = heap.trigger_stats();
        assert_eq!(stats.major_attempts, 2);
        assert_eq!(stats.major_successes, 2);
        assert_eq!(stats.forced_attempts, 2);
    }

    #[test]
    /// Descriptor policy is resolved before forced-minor selection, not after publication.
    fn forced_minor_observes_effective_old_only_allocation_policy() {
        let mut types = TypeRegistry::new();
        let object_type = types
            .try_register_old_only::<PinnedPayload>("PinnedPayload")
            .unwrap();
        let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
        let mut heap =
            Heap::with_trigger_config(HeapLimit::new(2 * SPAN_SIZE_BYTES), types, config);
        heap.set_forced_collection_mode(ForcedCollectionMode::Minor);
        let mut no_roots = Vec::<Value>::new();

        let reference = heap
            .try_allocate_with_gc(
                object_type,
                0,
                0,
                PinnedPayload,
                AllocationSpace::Young,
                &mut no_roots,
            )
            .unwrap();

        assert_eq!(
            heap.span_table()
                .metadata(reference.raw().span_id())
                .unwrap()
                .space(),
            SpanSpace::Old
        );
        assert_eq!(heap.trigger_stats().minor_attempts, 0);
    }

    #[test]
    /// Raw publication accrues byte debt while only the complete-root managed path repays it.
    fn raw_allocation_debt_triggers_the_next_managed_young_allocation() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<Value>("Value").unwrap();
        let config = GcTriggerConfig::new(32, usize::MAX, 100).unwrap();
        let mut heap =
            Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
        let mut root = heap
            .try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(1),
                AllocationSpace::Young,
            )
            .unwrap();
        assert_eq!(heap.trigger_stats().young_debt_bytes, 16);

        heap.try_allocate_with_gc(
            object_type,
            0,
            0,
            Value::from_i32(2),
            AllocationSpace::Young,
            &mut root,
        )
        .unwrap();

        assert!(heap.verify_reference(root.raw(), None).is_ok());
        let stats = heap.trigger_stats();
        assert_eq!(stats.minor_attempts, 1);
        assert_eq!(stats.young_debt_attempts, 1);
        assert_eq!(stats.young_debt_bytes, 16);
        assert_eq!(stats.young_allocated_bytes, 32);
    }

    #[test]
    /// Debt selects major first, then a distinct size class crosses the exact pressure boundary.
    fn old_debt_and_storage_pressure_select_full_major_collection() {
        let mut types = TypeRegistry::new();
        let value_type = types.try_register::<Value>("Value").unwrap();
        let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
        let config = GcTriggerConfig::new(usize::MAX, 32, 50).unwrap();
        let mut heap =
            Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
        let mut root = heap
            .try_allocate(value_type, 0, 0, Value::from_i32(1), AllocationSpace::Old)
            .unwrap();
        heap.try_allocate_with_gc(
            value_type,
            0,
            0,
            Value::from_i32(2),
            AllocationSpace::Old,
            &mut root,
        )
        .unwrap();
        assert_eq!(heap.trigger_stats().old_debt_attempts, 1);

        heap.try_allocate_with_gc(
            fanout_type,
            0,
            0,
            Fanout { edges: [None; 300] },
            AllocationSpace::Old,
            &mut root,
        )
        .unwrap();
        assert_eq!(heap.trigger_stats().heap_pressure_attempts, 1);
    }

    #[test]
    /// A hard-limit major rebuilds holes in the full active Old span before the single retry.
    fn managed_allocation_reuses_old_holes_after_hard_limit_collection() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<Value>("Value").unwrap();
        let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
        let mut heap = Heap::with_trigger_config(HeapLimit::new(SPAN_SIZE_BYTES), types, config);
        let mut root = heap
            .try_allocate(object_type, 0, 0, Value::from_i32(0), AllocationSpace::Old)
            .unwrap();
        let span = root.raw().span_id();
        let slot_count = heap
            .span_table()
            .metadata(span)
            .unwrap()
            .size_class()
            .slot_count();
        for value in 1..slot_count {
            heap.try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(i32::from(value)),
                AllocationSpace::Old,
            )
            .unwrap();
        }

        let allocated = heap
            .try_allocate_with_gc(
                object_type,
                0,
                0,
                Value::from_i32(7),
                AllocationSpace::Old,
                &mut root,
            )
            .unwrap();

        assert_eq!(allocated.raw().span_id(), span);
        assert!(heap.verify_reference(root.raw(), None).is_ok());
        assert_eq!(heap.committed_span_storage_bytes(), SPAN_SIZE_BYTES);
        assert_eq!(heap.trigger_stats().heap_limit_attempts, 1);
    }

    #[test]
    /// Repeated host notifications coalesce without turning later allocations into polling points.
    fn memory_pressure_commands_coalesce_and_are_consumed_by_one_managed_allocation() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<Value>("Value").unwrap();
        let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
        let mut heap =
            Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
        heap.request_memory_pressure_collection();
        heap.request_memory_pressure_collection();
        let mut no_roots = Vec::<Value>::new();

        for value in 0..2 {
            heap.try_allocate_with_gc(
                object_type,
                0,
                0,
                Value::from_i32(value),
                AllocationSpace::Young,
                &mut no_roots,
            )
            .unwrap();
        }

        let stats = heap.trigger_stats();
        assert_eq!(stats.memory_pressure_requests, 2);
        assert_eq!(stats.memory_pressure_commands_consumed, 1);
        assert_eq!(stats.major_attempts, 1);
    }

    #[test]
    /// Fixed-seed graph churn crosses every Phase 1B collection and lifetime boundary repeatedly.
    fn randomized_forced_collection_stress_preserves_exact_graph_contracts() {
        const STRESS_STEPS: usize = 96;
        const STRESS_SEED: u64 = 0x6a09_e667_f3bc_c909;

        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let weak_type = types.try_register::<WeakHolder>("WeakHolder").unwrap();
        let ephemeron_type = types
            .try_register::<EphemeronHolder>("EphemeronHolder")
            .unwrap();
        let finalization_type = types
            .try_register::<FinalizationHolder>("FinalizationHolder")
            .unwrap();
        let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
        let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
        let mut heap =
            Heap::with_trigger_config(HeapLimit::new(128 * SPAN_SIZE_BYTES), types, config);
        let mut roots = StressRoots::new();

        let key = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let ephemeron_value = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let weak_target = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let finalization_target = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let held = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        roots
            .nodes
            .extend([key, ephemeron_value, weak_target, finalization_target, held]);
        let weak_holder = heap
            .try_allocate(
                weak_type,
                0,
                0,
                WeakHolder {
                    target: WeakGcRef::new(weak_target),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let ephemeron_holder = heap
            .try_allocate(
                ephemeron_type,
                0,
                0,
                EphemeronHolder {
                    entry: Ephemeron::new(key, Value::from_heap_ref(ephemeron_value.raw())),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let finalization_holder = heap
            .try_allocate(
                finalization_type,
                0,
                0,
                FinalizationHolder {
                    registration: FinalizationRegistration::new(
                        finalization_target,
                        Value::from_heap_ref(held.raw()),
                    ),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        roots.stable.extend([
            Value::from_heap_ref(key.raw()),
            Value::from_heap_ref(weak_holder.raw()),
            Value::from_heap_ref(ephemeron_holder.raw()),
            Value::from_heap_ref(finalization_holder.raw()),
        ]);
        roots.nodes.clear();

        heap.set_forced_collection_mode(ForcedCollectionMode::Major);
        let first_cycle_node = heap
            .try_allocate_with_gc(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
                &mut roots,
            )
            .unwrap();
        roots.nodes.push(first_cycle_node);
        assert_eq!(heap.finalization_queue_stats().pending, 1);

        let weak_was_cleared = heap.with_running_scope(|scope| {
            let holder = scope.root(weak_holder).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(holder, weak_type)
                    .unwrap()
                    .target
                    .get()
                    .is_none()
            })
        });
        assert!(weak_was_cleared);
        assert!(heap.verify_reference(ephemeron_value.raw(), None).is_ok());
        assert!(heap.verify_reference(held.raw(), None).is_ok());

        let mut rng = DeterministicRng(STRESS_SEED);
        let attempts_before = heap.trigger_stats();
        for _ in 0..STRESS_STEPS {
            let mode = if rng.next().is_multiple_of(3) {
                ForcedCollectionMode::Major
            } else {
                ForcedCollectionMode::Minor
            };
            heap.set_forced_collection_mode(mode);
            let source = roots.nodes[(rng.next() as usize) % roots.nodes.len()];
            let allocated = heap
                .try_allocate_with_gc(
                    node_type,
                    0,
                    0,
                    ChainNode { next: Some(source) },
                    AllocationSpace::Young,
                    &mut roots,
                )
                .unwrap();
            heap.with_running_scope(|scope| {
                let source = scope.root(source).unwrap();
                let allocated_local = scope.root(allocated).unwrap();
                scope.with_no_gc_scope(|no_gc| {
                    no_gc.borrow_mut(source, node_type).unwrap().next = Some(allocated);
                });
                scope.write_barrier(source, allocated_local).unwrap();
            });
            if roots.nodes.len() == roots.nodes.capacity() {
                let remove = (rng.next() as usize) % roots.nodes.len();
                roots.nodes.swap_remove(remove);
            }
            roots.nodes.push(allocated);
            for root in &roots.nodes {
                assert!(heap.verify_reference(root.raw(), None).is_ok());
            }
            heap.verify_generational_barriers().unwrap();
        }
        let attempts_after = heap.trigger_stats();
        assert!(attempts_after.minor_attempts > attempts_before.minor_attempts);
        assert!(attempts_after.major_attempts > attempts_before.major_attempts);

        let anchor = roots.nodes[0];
        heap.set_forced_collection_mode(ForcedCollectionMode::Minor);
        for _ in 0..2 {
            heap.try_allocate_with_gc(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
                &mut roots,
            )
            .unwrap();
        }
        assert_eq!(
            heap.span_table().reference_space(anchor.raw()).unwrap(),
            crate::table::ReferenceSpace::OldSmall
        );

        heap.set_forced_collection_mode(ForcedCollectionMode::Major);
        let released_span = heap
            .try_allocate_with_gc(
                fanout_type,
                0,
                0,
                Fanout { edges: [None; 300] },
                AllocationSpace::Young,
                &mut roots,
            )
            .unwrap()
            .raw()
            .span_id();
        let reused_span = heap
            .try_allocate_with_gc(
                fanout_type,
                0,
                0,
                Fanout { edges: [None; 300] },
                AllocationSpace::Young,
                &mut roots,
            )
            .unwrap()
            .raw()
            .span_id();
        assert_eq!(reused_span, released_span);

        let mut low_types = TypeRegistry::new();
        let large_type = low_types
            .try_register::<LargePayload>("LargePayload")
            .unwrap();
        let mut low_heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), low_types);
        let mut no_roots = Vec::<Value>::new();
        assert!(matches!(
            low_heap.try_allocate_with_gc(
                large_type,
                0,
                0,
                LargePayload {
                    _bytes: [0; 70_000]
                },
                AllocationSpace::Old,
                &mut no_roots,
            ),
            Err(ManagedAllocationError::Allocation(
                HeapAllocationError::HeapLimitExceeded { .. }
            ))
        ));
        assert_eq!(low_heap.trigger_stats().heap_limit_attempts, 1);
    }

    #[test]
    /// Fills the 16-byte class exactly and proves the next slow path returns a typed limit error.
    fn full_active_span_obeys_the_configured_storage_limit() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<Value>("Value").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let first = heap
            .try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(0),
                AllocationSpace::Young,
            )
            .unwrap();
        let span = first.raw().span_id();
        let slot_count = heap
            .span_table()
            .metadata(span)
            .unwrap()
            .size_class()
            .slot_count();
        for value in 1..slot_count {
            heap.try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(i32::from(value)),
                AllocationSpace::Young,
            )
            .unwrap();
        }

        assert_eq!(
            heap.try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(-1),
                AllocationSpace::Young
            ),
            Err(HeapAllocationError::HeapLimitExceeded {
                limit: SPAN_SIZE_BYTES,
                committed: SPAN_SIZE_BYTES,
                requested: SPAN_SIZE_BYTES,
            })
        );
        assert_eq!(heap.span_table().live_spans(), 1);
    }

    #[test]
    fn heap_rejects_a_typed_token_not_registered_at_its_header_id() {
        let mut first_registry = TypeRegistry::new();
        let object_type = first_registry.try_register::<Value>("Value").unwrap();
        let mut conflicting_registry = TypeRegistry::new();
        let conflicting_type = conflicting_registry
            .try_register::<OtherPayload>("OtherPayload")
            .unwrap();
        assert_eq!(object_type.type_id(), conflicting_type.type_id());
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), conflicting_registry);

        assert_eq!(
            heap.try_allocate(
                object_type,
                0,
                0,
                Value::from_i32(1),
                AllocationSpace::Young
            ),
            Err(HeapAllocationError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            })
        );
        assert_eq!(heap.committed_span_storage_bytes(), 0);
    }

    #[test]
    /// Spans a continuation ID, verifies the owner, and rejects an interior logical reference.
    fn large_objects_allocate_directly_in_contiguous_old_ranges() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<LargePayload>("LargePayload").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let reference = heap
            .try_allocate(
                object_type,
                7,
                11,
                LargePayload {
                    _bytes: [0; 70_000],
                },
                AllocationSpace::Young,
            )
            .unwrap();

        assert_eq!(reference.raw().span_id().index(), 0);
        assert_eq!(reference.raw().span_offset().get(), 16);
        assert_eq!(heap.committed_span_storage_bytes(), 2 * SPAN_SIZE_BYTES);
        assert_eq!(heap.span_table().live_spans(), 2);
        assert_eq!(
            heap.span_table()
                .large_metadata(reference.raw().span_id())
                .unwrap()
                .span_count(),
            2
        );
        let header = heap
            .verify_reference(reference.raw(), Some(object_type.type_id()))
            .unwrap();
        assert_eq!(header.flags(), 7);
        assert_eq!(header.aux(), 11);

        let continuation =
            RawHeapRef::from_parts(crate::SpanId::new(1), crate::SpanOffset::new(16).unwrap());
        assert_eq!(
            heap.verify_reference(continuation, None),
            Err(HeapReferenceError::LargeContinuationReference {
                reference: continuation,
                owner: reference.raw().span_id(),
                ordinal: 1,
            })
        );
        assert_eq!(
            heap.span_table()
                .base_address(crate::SpanId::new(1))
                .unwrap() as usize
                - heap
                    .span_table()
                    .base_address(reference.raw().span_id())
                    .unwrap() as usize,
            SPAN_SIZE_BYTES
        );

        let reclaimed = heap.reclaim_large_after_drop(reference.raw()).unwrap();
        assert_eq!(reclaimed.span_count(), 2);
        assert_eq!(reclaimed.storage_bytes(), 2 * SPAN_SIZE_BYTES);
        assert_eq!(heap.committed_span_storage_bytes(), 0);
        assert_eq!(heap.span_table().live_spans(), 0);
        let reused = heap
            .try_allocate(
                object_type,
                0,
                0,
                LargePayload {
                    _bytes: [0; 70_000],
                },
                AllocationSpace::Old,
            )
            .unwrap();
        assert_eq!(reused.raw(), reference.raw());
        assert_eq!(heap.span_table().historical_span_count(), 2);
    }

    #[test]
    fn large_object_limit_failure_does_not_publish_owner_or_continuations() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<LargePayload>("LargePayload").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        assert_eq!(
            heap.try_allocate(
                object_type,
                0,
                0,
                LargePayload {
                    _bytes: [0; 70_000],
                },
                AllocationSpace::Old,
            ),
            Err(HeapAllocationError::HeapLimitExceeded {
                limit: SPAN_SIZE_BYTES,
                committed: 0,
                requested: 2 * SPAN_SIZE_BYTES,
            })
        );
        assert_eq!(heap.span_table().live_spans(), 0);
        assert_eq!(heap.span_table().historical_span_count(), 0);
    }

    #[test]
    /// Proves host backing charges cannot bypass spans and invalid releases do not underflow.
    fn external_backing_bytes_share_the_hard_limit_and_release_exactly() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<Value>("Value").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES + 32), types);
        heap.try_charge_external(32).unwrap();
        heap.try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(1),
            AllocationSpace::Young,
        )
        .unwrap();

        assert_eq!(heap.external_bytes(), 32);
        assert_eq!(heap.committed_heap_bytes(), SPAN_SIZE_BYTES + 32);
        assert_eq!(
            heap.try_charge_external(1),
            Err(HeapAllocationError::HeapLimitExceeded {
                limit: SPAN_SIZE_BYTES + 32,
                committed: SPAN_SIZE_BYTES + 32,
                requested: 1,
            })
        );
        assert!(!heap.release_external(33));
        assert!(heap.release_external(32));
        assert_eq!(heap.external_bytes(), 0);
    }

    #[test]
    /// A 10,000-object chain reaches its fixed point with a gray peak of one, proving iteration.
    fn strong_marking_does_not_recurse_through_the_native_stack() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(1024 * 1024), types);
        let mut tail = None;
        for _ in 0..10_000 {
            tail = Some(
                heap.try_allocate(
                    node_type,
                    0,
                    0,
                    ChainNode { next: tail },
                    AllocationSpace::Old,
                )
                .unwrap(),
            );
        }
        let mut root = tail.expect("chain is non-empty");

        let stats = heap.mark_strong(&mut root).unwrap();

        assert_eq!(stats.marked_objects, 10_000);
        assert_eq!(stats.traced_objects, 10_000);
        assert_eq!(stats.traced_edges, 10_000);
        assert_eq!(heap.gray_queue_stats().peak_len, 1);
        assert_eq!(heap.gray_queue_stats().initial_capacity, 256);
        assert_eq!(heap.gray_queue_stats().growth_count, 0);
    }

    #[test]
    /// A broad graph crosses the initial queue guess once and retains the measured high water.
    fn strong_marking_records_bounded_gray_queue_growth() {
        let mut types = TypeRegistry::new();
        let leaf_type = types.try_register::<Leaf>("Leaf").unwrap();
        let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
        let mut heap = Heap::new(HeapLimit::new(1024 * 1024), types);
        let mut edges = [None; 300];
        for edge in &mut edges[..299] {
            *edge = Some(
                heap.try_allocate(leaf_type, 0, 0, Leaf, AllocationSpace::Old)
                    .unwrap(),
            );
        }
        edges[299] = edges[0];
        let mut root = heap
            .try_allocate(fanout_type, 0, 0, Fanout { edges }, AllocationSpace::Old)
            .unwrap();

        let stats = heap.mark_strong(&mut root).unwrap();
        let queue = heap.gray_queue_stats();

        assert_eq!(stats.marked_objects, 300);
        assert_eq!(stats.traced_objects, 300);
        assert_eq!(stats.traced_edges, 301);
        assert_eq!(queue.initial_capacity, 256);
        assert_eq!(queue.growth_count, 1);
        assert_eq!(queue.peak_len, 299);
        assert!(queue.retained_capacity >= queue.peak_len);
    }

    #[test]
    /// Conservative Old initialization discovers young edges, then exact rebuilding clears them.
    fn young_mark_rebuilds_small_remembered_cards() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let young = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let old = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: Some(young) },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let retained = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(retained.mark.marked_objects, 1);
        assert_eq!(retained.dirty_cards_scanned, 1);
        assert_eq!(retained.old_objects_scanned, 1);

        heap.with_running_scope(|scope| {
            let old = scope.root(old).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(old, node_type).unwrap().next = None;
            });
        });
        let cleared = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(cleared.dirty_cards_scanned, 1);
        assert_eq!(cleared.old_objects_scanned, 1);
        assert_eq!(cleared.mark.marked_objects, 0);
        let skipped = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(skipped.dirty_cards_scanned, 0);
        assert_eq!(skipped.old_objects_scanned, 0);
    }

    #[test]
    /// A clean Old object enters the remembered set only after its explicit post-write barrier.
    fn old_to_young_write_barrier_dirties_a_clean_source_card() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let old = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();
        heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(
            heap.mark_young(&mut no_roots).unwrap().dirty_cards_scanned,
            0
        );
        let young = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();

        heap.with_running_scope(|scope| {
            let old_local = scope.root(old).unwrap();
            let young_local = scope.root(young).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(old_local, node_type).unwrap().next = Some(young);
            });
            assert!(scope.write_barrier(old_local, young_local).unwrap());
        });

        let stats = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(stats.dirty_cards_scanned, 1);
        assert_eq!(stats.old_objects_scanned, 1);
        assert_eq!(stats.mark.marked_objects, 1);
    }

    #[test]
    /// Fault injection distinguishes a missing card from a dirty owner absent from the chain.
    fn barrier_verifier_rejects_small_card_and_intrusive_chain_omissions() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
        let old = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();
        heap.mark_young(&mut no_roots).unwrap();
        let young = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        heap.with_running_scope(|scope| {
            let old = scope.root(old).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(old, node_type).unwrap().next = Some(young);
            });
        });

        let missing_card = BarrierVerificationError::MissingSmallCard {
            source: old.raw(),
            target: young.raw(),
        };
        assert_eq!(heap.verify_generational_barriers(), Err(missing_card));
        assert_eq!(
            heap.collect_minor(&mut no_roots),
            Err(MinorCollectionError::Barrier(missing_card))
        );
        assert!(heap.write_barrier(old.raw(), young.raw()).unwrap());
        assert_eq!(
            heap.verify_generational_barriers()
                .unwrap()
                .small_card_edges,
            1
        );

        heap.with_running_scope(|scope| {
            let old = scope.root(old).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(old, node_type).unwrap().next = None;
            });
        });
        heap.mark_young(&mut no_roots).unwrap();
        heap.with_running_scope(|scope| {
            let old = scope.root(old).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(old, node_type).unwrap().next = Some(young);
            });
        });
        let mut cards = CardBitmap::new();
        cards.mark(old.raw().span_offset());
        heap.span_table_mut()
            .replace_old_cards(old.raw().span_id(), cards);
        assert_eq!(
            heap.verify_generational_barriers(),
            Err(BarrierVerificationError::MissingRememberedSource {
                source: old.raw(),
                target: young.raw(),
            })
        );
    }

    #[test]
    /// Large owners require both their conservative owner bit and remembered-chain membership.
    fn barrier_verifier_rejects_missing_large_owner_state() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let large_type = types
            .try_register::<LargeEdgeNode>("LargeEdgeNode")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(5 * SPAN_SIZE_BYTES), types);
        let old = heap
            .try_allocate(
                large_type,
                0,
                0,
                LargeEdgeNode {
                    _bytes: [0; 70_000],
                    next: None,
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();
        heap.mark_young(&mut no_roots).unwrap();
        let young = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        heap.with_running_scope(|scope| {
            let old = scope.root(old).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(old, large_type).unwrap().next = Some(young);
            });
        });

        assert_eq!(
            heap.verify_generational_barriers(),
            Err(BarrierVerificationError::MissingLargeRememberedBit {
                source: old.raw(),
                target: young.raw(),
            })
        );
        assert!(heap.write_barrier(old.raw(), young.raw()).unwrap());
        let stats = heap.verify_generational_barriers().unwrap();
        assert_eq!(stats.large_owner_edges, 1);
        assert_eq!(stats.old_to_young_edges, 1);
    }

    #[test]
    /// Direct-old large objects use owner-level remembered state without allocating card arrays.
    fn young_mark_scans_and_rebuilds_remembered_large_owners() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let large_type = types
            .try_register::<LargeEdgeNode>("LargeEdgeNode")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
        let young = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let large = heap
            .try_allocate(
                large_type,
                0,
                0,
                LargeEdgeNode {
                    _bytes: [0; 70_000],
                    next: Some(young),
                },
                AllocationSpace::Young,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let stats = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(stats.remembered_large_owners_scanned, 1);
        assert_eq!(stats.mark.marked_objects, 1);

        heap.with_running_scope(|scope| {
            let large = scope.root(large).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(large, large_type).unwrap().next = None;
            });
        });
        let cleared = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(cleared.remembered_large_owners_scanned, 1);
        assert_eq!(cleared.mark.marked_objects, 0);
        let skipped = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(skipped.remembered_large_owners_scanned, 0);
    }

    #[test]
    /// A failed young mark keeps the original dirty card so a repaired source is rescanned.
    fn young_mark_error_preserves_remembered_state() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let invalid = RawHeapRef::new(u32::MAX).unwrap();
        // SAFETY: this deliberately forged typed edge is never dereferenced; the test proves exact
        // young marking rejects it and retains conservative remembered metadata on the error path.
        let invalid = unsafe { GcRef::<ChainNode>::from_raw_unchecked(invalid) };
        let old = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode {
                    next: Some(invalid),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();
        assert!(matches!(
            heap.mark_young(&mut no_roots),
            Err(crate::MarkError::InvalidReference(_))
        ));

        heap.with_running_scope(|scope| {
            let old = scope.root(old).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(old, node_type).unwrap().next = None;
            });
        });
        let repaired = heap.mark_young(&mut no_roots).unwrap();
        assert_eq!(repaired.dirty_cards_scanned, 1);
        assert_eq!(repaired.old_objects_scanned, 1);
    }

    #[test]
    /// Full major clears a dead weak target before sweep invalidates its allocation.
    fn full_major_clears_weak_edges_without_retaining_targets() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let weak_type = types.try_register::<WeakHolder>("WeakHolder").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let target = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut holder = heap
            .try_allocate(
                weak_type,
                0,
                0,
                WeakHolder {
                    target: WeakGcRef::new(target),
                },
                AllocationSpace::Old,
            )
            .unwrap();

        let stats = heap.collect_major(&mut holder).unwrap();
        assert_eq!(stats.mark.weak_owners, 1);
        assert_eq!(stats.mark.weak_slots_cleared, 1);
        assert_eq!(stats.sweep.reclaimed_objects, 1);
        let weak_capacity = heap.weak_owner_stats();
        assert_eq!(weak_capacity.current_len, 1);
        assert_eq!(weak_capacity.initial_capacity, 64);
        assert!(matches!(
            heap.verify_reference(target.raw(), None),
            Err(HeapReferenceError::UnallocatedSlot(_))
        ));
        let cleared = heap.with_running_scope(|scope| {
            let holder = scope.root(holder).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(holder, weak_type)
                    .unwrap()
                    .target
                    .get()
                    .is_none()
            })
        });
        assert!(cleared);
    }

    #[test]
    /// Reversed ephemeron owners require a second pass to propagate key liveness to the leaf.
    fn full_major_reaches_ephemeron_fixed_point() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let ephemeron_type = types
            .try_register::<EphemeronHolder>("EphemeronHolder")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let key = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let second_key = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let leaf = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let first = heap
            .try_allocate(
                ephemeron_type,
                0,
                0,
                EphemeronHolder {
                    entry: Ephemeron::new(key, Value::from_heap_ref(second_key.raw())),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let second = heap
            .try_allocate(
                ephemeron_type,
                0,
                0,
                EphemeronHolder {
                    entry: Ephemeron::new(second_key, Value::from_heap_ref(leaf.raw())),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut roots = vec![
            Value::from_heap_ref(first.raw()),
            Value::from_heap_ref(second.raw()),
            Value::from_heap_ref(key.raw()),
        ];

        let stats = heap.collect_major(&mut roots).unwrap();
        assert!(stats.mark.ephemeron_passes >= 2);
        assert_eq!(stats.mark.ephemeron_values_marked, 2);
        assert_eq!(stats.mark.ephemerons_cleared, 0);
        assert_eq!(stats.sweep.live_objects, 5);
        assert!(heap.verify_reference(leaf.raw(), None).is_ok());
    }

    #[test]
    /// A dead ephemeron key clears both entry fields and permits its value to be swept.
    fn full_major_clears_dead_ephemerons() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let ephemeron_type = types
            .try_register::<EphemeronHolder>("EphemeronHolder")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let key = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let value = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut holder = heap
            .try_allocate(
                ephemeron_type,
                0,
                0,
                EphemeronHolder {
                    entry: Ephemeron::new(key, Value::from_heap_ref(value.raw())),
                },
                AllocationSpace::Old,
            )
            .unwrap();

        let stats = heap.collect_major(&mut holder).unwrap();
        assert_eq!(stats.mark.ephemerons_cleared, 1);
        assert_eq!(stats.sweep.reclaimed_objects, 2);
        let cleared = heap.with_running_scope(|scope| {
            let holder = scope.root(holder).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                let entry = &no_gc.borrow(holder, ephemeron_type).unwrap().entry;
                entry.key().is_none()
                    && entry.value().as_immediate() == Some(tachyon_value::Immediate::Undefined)
            })
        });
        assert!(cleared);
    }

    #[test]
    /// Minor clearing discovers a weak Old owner through its card and reclaims only the young target.
    fn minor_clears_old_to_young_weak_edges() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let weak_type = types.try_register::<WeakHolder>("WeakHolder").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let target = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let holder = heap
            .try_allocate(
                weak_type,
                0,
                0,
                WeakHolder {
                    target: WeakGcRef::new(target),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let stats = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(stats.mark.mark.weak_slots_cleared, 1);
        assert_eq!(stats.sweep.sweep.reclaimed_objects, 1);
        assert!(heap.verify_reference(holder.raw(), None).is_ok());
        let cleared = heap.with_running_scope(|scope| {
            let holder = scope.root(holder).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(holder, weak_type)
                    .unwrap()
                    .target
                    .get()
                    .is_none()
            })
        });
        assert!(cleared);
    }

    #[test]
    /// Minor ephemeron closure treats every Old key as live and retains its young value.
    fn minor_ephemeron_with_old_key_marks_young_value() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let ephemeron_type = types
            .try_register::<EphemeronHolder>("EphemeronHolder")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
        let key = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let value = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        heap.try_allocate(
            ephemeron_type,
            0,
            0,
            EphemeronHolder {
                entry: Ephemeron::new(key, Value::from_heap_ref(value.raw())),
            },
            AllocationSpace::Old,
        )
        .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let stats = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(stats.mark.mark.ephemeron_values_marked, 1);
        assert_eq!(stats.mark.mark.ephemerons_cleared, 0);
        assert!(heap.verify_reference(value.raw(), None).is_ok());
        assert_eq!(
            heap.span_table()
                .metadata(value.raw().span_id())
                .unwrap()
                .space(),
            SpanSpace::Survivor { age: 1 }
        );
    }

    #[test]
    /// A dead young key clears its Old ephemeron owner and permits both young objects to die.
    fn minor_clears_ephemeron_with_dead_young_key() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let ephemeron_type = types
            .try_register::<EphemeronHolder>("EphemeronHolder")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
        let key = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let value = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let holder = heap
            .try_allocate(
                ephemeron_type,
                0,
                0,
                EphemeronHolder {
                    entry: Ephemeron::new(key, Value::from_heap_ref(value.raw())),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let stats = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(stats.mark.mark.ephemerons_cleared, 1);
        assert_eq!(stats.sweep.sweep.reclaimed_objects, 2);
        let cleared = heap.with_running_scope(|scope| {
            let holder = scope.root(holder).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                let entry = &no_gc.borrow(holder, ephemeron_type).unwrap().entry;
                entry.key().is_none()
                    && entry.value().as_immediate() == Some(tachyon_value::Immediate::Undefined)
            })
        });
        assert!(cleared);
    }

    #[test]
    /// AddToKeptObjects survives collections until the host explicitly ends the current job.
    fn kept_objects_are_job_scoped_precise_roots() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let target = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        heap.with_running_scope(|scope| {
            let target = scope.root(target).unwrap();
            assert!(scope.keep_alive(target).unwrap());
            assert!(!scope.keep_alive(target).unwrap());
        });
        let mut no_roots = Vec::<Value>::new();

        let retained = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(retained.sweep.live_objects, 1);
        assert_eq!(heap.kept_object_stats().current_len, 1);
        assert_eq!(heap.kept_object_stats().initial_capacity, 64);
        heap.clear_kept_objects_at_job_boundary();
        let released = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(released.sweep.reclaimed_objects, 1);
        assert_eq!(heap.kept_object_stats().current_len, 0);
    }

    #[test]
    /// Dead finalization targets enqueue cleanup before sweep; queued registry/value stay rooted.
    fn finalization_queue_roots_cleanup_until_safepoint_consumption() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let registry_type = types
            .try_register::<FinalizationHolder>("FinalizationHolder")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let target = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let held = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut registry = heap
            .try_allocate(
                registry_type,
                0,
                0,
                FinalizationHolder {
                    registration: FinalizationRegistration::new(
                        target,
                        Value::from_heap_ref(held.raw()),
                    ),
                },
                AllocationSpace::Old,
            )
            .unwrap();

        let first = heap.collect_major(&mut registry).unwrap();
        assert_eq!(first.mark.finalizations_enqueued, 1);
        assert_eq!(first.sweep.reclaimed_objects, 1);
        assert_eq!(heap.finalization_queue_stats().pending, 1);
        assert_eq!(heap.finalization_queue_stats().initial_capacity, 64);
        let mut no_roots = Vec::<Value>::new();
        let queued = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(queued.sweep.live_objects, 2);
        let record = heap.pop_pending_finalization().unwrap();
        assert_eq!(record.registry(), registry.raw());
        assert_eq!(record.held_value().as_heap_ref(), Some(held.raw()));
        let drained = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drained.sweep.reclaimed_objects, 2);
    }

    #[test]
    /// Minor finalization enqueues an Old registry's dead young target before young sweep.
    fn minor_enqueues_dead_young_finalization_targets() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let registry_type = types
            .try_register::<FinalizationHolder>("FinalizationHolder")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let target = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let registry = heap
            .try_allocate(
                registry_type,
                0,
                0,
                FinalizationHolder {
                    registration: FinalizationRegistration::new(target, Value::from_i32(7)),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let stats = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(stats.mark.mark.finalizations_enqueued, 1);
        assert_eq!(stats.sweep.sweep.reclaimed_objects, 1);
        let record = heap.pop_pending_finalization().unwrap();
        assert_eq!(record.registry(), registry.raw());
        assert_eq!(record.held_value().as_i32(), Some(7));
    }

    #[test]
    /// Descriptor policy overrides a mistaken Young request for pinned/finalizer payloads.
    fn old_only_type_policy_cannot_allocate_into_eden() {
        let mut types = TypeRegistry::new();
        let pinned_type = types
            .try_register_old_only::<PinnedPayload>("PinnedPayload")
            .unwrap();
        assert!(matches!(
            types.try_register::<PinnedPayload>("WrongPolicy"),
            Err(TypeRegistrationError::AllocationPolicyMismatch)
        ));
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let pinned = heap
            .try_allocate(pinned_type, 0, 0, PinnedPayload, AllocationSpace::Young)
            .unwrap();
        assert_eq!(
            heap.span_table()
                .metadata(pinned.raw().span_id())
                .unwrap()
                .space(),
            SpanSpace::Old
        );
    }

    #[test]
    /// Minor sweep reclaims white slots, ages survivors, promotes in place, and exposes old holes.
    fn minor_collection_promotes_without_moving_and_reuses_dead_holes() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<DropNode>("DropNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let mut root = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Young,
            )
            .unwrap();
        let dead = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Young,
            )
            .unwrap();
        let native_before = heap
            .span_table()
            .base_address(root.raw().span_id())
            .unwrap()
            .wrapping_add(usize::from(root.raw().span_offset().get()));

        let first = heap.collect_minor(&mut root).unwrap();
        assert_eq!(first.sweep.sweep.live_objects, 1);
        assert_eq!(first.sweep.sweep.reclaimed_objects, 1);
        assert_eq!(first.sweep.eden_to_survivor, 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            heap.span_table()
                .metadata(root.raw().span_id())
                .unwrap()
                .space(),
            SpanSpace::Survivor { age: 1 }
        );

        let second = heap.collect_minor(&mut root).unwrap();
        assert_eq!(second.mark.promotion_objects_scanned, 1);
        assert_eq!(second.sweep.whole_span_promotions, 1);
        assert_eq!(
            heap.span_table()
                .metadata(root.raw().span_id())
                .unwrap()
                .space(),
            SpanSpace::Old
        );
        let native_after = heap
            .span_table()
            .base_address(root.raw().span_id())
            .unwrap()
            .wrapping_add(usize::from(root.raw().span_offset().get()));
        assert_eq!(native_after, native_before);
        assert_eq!(root.raw().span_id(), dead.raw().span_id());

        let reused = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        assert_eq!(reused.raw(), dead.raw());
        let mut no_roots = Vec::<Value>::new();
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    /// Releasing an empty Eden span repairs its active cache and permits stable logical ID reuse.
    fn minor_collection_releases_empty_eden_and_repairs_active_cache() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let dead = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let stats = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(stats.sweep.sweep.reclaimed_objects, 1);
        assert_eq!(stats.sweep.sweep.spans_released, 1);
        assert_eq!(heap.committed_span_storage_bytes(), 0);
        let reused = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        assert_eq!(reused.raw(), dead.raw());
    }

    #[test]
    /// Promotion prepares remembered cards before the source becomes Old in the sweep phase.
    fn promoted_span_remembers_edges_to_younger_spans() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let mut parent = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        heap.collect_minor(&mut parent).unwrap();
        let child = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        heap.with_running_scope(|scope| {
            let parent = scope.root(parent).unwrap();
            let child_local = scope.root(child).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(parent, node_type).unwrap().next = Some(child);
            });
            assert!(!scope.write_barrier(parent, child_local).unwrap());
        });

        let promotion = heap.collect_minor(&mut parent).unwrap();
        assert_eq!(promotion.sweep.whole_span_promotions, 1);
        assert_eq!(promotion.mark.promotion_objects_scanned, 1);
        assert_eq!(
            heap.span_table()
                .metadata(parent.raw().span_id())
                .unwrap()
                .space(),
            SpanSpace::Old
        );
        assert_eq!(
            heap.span_table()
                .metadata(child.raw().span_id())
                .unwrap()
                .space(),
            SpanSpace::Survivor { age: 1 }
        );

        let mut no_roots = Vec::<Value>::new();
        let remembered = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(remembered.mark.dirty_cards_scanned, 1);
        assert_eq!(remembered.mark.old_objects_scanned, 1);
        assert_eq!(remembered.mark.mark.marked_objects, 1);
        assert!(heap.verify_reference(child.raw(), None).is_ok());
    }

    #[test]
    /// Minor sweep never reclaims Old payloads even when no precise root reaches them.
    fn minor_collection_sweeps_only_young_spans() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<DropNode>("DropNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let old = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        heap.try_allocate(
            node_type,
            0,
            0,
            DropNode {
                next: None,
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Young,
        )
        .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let minor = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(minor.sweep.sweep.scanned_objects, 1);
        assert_eq!(minor.sweep.sweep.reclaimed_objects, 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(heap.verify_reference(old.raw(), None).is_ok());
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    /// Repeated empty minors reuse one stable entry without duplicate intrusive-list membership.
    fn repeated_empty_minor_collections_keep_young_chain_bounded() {
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let mut first = None;
        let mut no_roots = Vec::<Value>::new();
        for _ in 0..256 {
            let reference = heap
                .try_allocate(
                    node_type,
                    0,
                    0,
                    ChainNode { next: None },
                    AllocationSpace::Young,
                )
                .unwrap();
            let expected = *first.get_or_insert(reference.raw());
            assert_eq!(reference.raw(), expected);
            let stats = heap.collect_minor(&mut no_roots).unwrap();
            assert_eq!(stats.sweep.sweep.spans_processed, 1);
            assert_eq!(stats.sweep.sweep.spans_released, 1);
        }
        assert_eq!(heap.span_table().historical_span_count(), 1);
        assert_eq!(heap.span_table().live_spans(), 0);
    }

    #[test]
    /// Keeps an exact graph, reclaims one peer, and proves the rebuilt Old free list reuses its slot.
    fn full_major_preserves_roots_and_reuses_reclaimed_old_slots() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<DropNode>("DropNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let child = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut root = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: Some(child),
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let dead = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();

        let stats = heap.collect_major(&mut root).unwrap();
        assert_eq!(stats.sweep.scanned_objects, 3);
        assert_eq!(stats.sweep.live_objects, 2);
        assert_eq!(stats.sweep.reclaimed_objects, 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(heap.verify_reference(root.raw(), None).is_ok());
        assert!(heap.verify_reference(child.raw(), None).is_ok());
        assert_eq!(
            heap.verify_reference(dead.raw(), None),
            Err(HeapReferenceError::UnallocatedSlot(dead.raw()))
        );

        let reused = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        assert_eq!(reused.raw(), dead.raw());

        let mut no_roots = Vec::<Value>::new();
        let final_stats = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(final_stats.sweep.reclaimed_objects, 3);
        assert_eq!(final_stats.sweep.spans_released, 1);
        assert_eq!(drops.load(Ordering::Relaxed), 4);
        assert_eq!(heap.committed_span_storage_bytes(), 0);
    }

    #[test]
    /// Builds a two-node cycle through a validated payload boundary; reachability, not ref counts, wins.
    fn full_major_handles_reachable_and_unreachable_cycles() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<DropNode>("DropNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let first = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let second = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: Some(first),
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let descriptor = heap.types.descriptor(node_type.type_id()).unwrap();
        let first_payload = heap.table.payload_address(first.raw(), descriptor).unwrap();
        // SAFETY: table verification paired this payload with `DropNode`; collection and allocation
        // are paused while this exclusive test-only mutation installs the back edge.
        unsafe { first_payload.cast::<DropNode>().as_mut().next = Some(second) };
        let mut root = first;

        let live = heap.collect_major(&mut root).unwrap();
        assert_eq!(live.sweep.live_objects, 2);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        let mut no_roots = Vec::<Value>::new();
        let dead = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(dead.sweep.reclaimed_objects, 2);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    /// Reclaims one independently backed large range and invokes its descriptor drop exactly once.
    fn full_major_reclaims_large_owner_and_continuations() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let large_type = types.try_register::<DropLarge>("DropLarge").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        let reference = heap
            .try_allocate(
                large_type,
                0,
                0,
                DropLarge {
                    _bytes: [0; 70_000],
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Young,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let stats = heap.collect_major(&mut no_roots).unwrap();

        assert_eq!(stats.sweep.scanned_objects, 1);
        assert_eq!(stats.sweep.reclaimed_objects, 1);
        assert_eq!(stats.sweep.spans_processed, 1);
        assert_eq!(stats.sweep.spans_released, 2);
        assert_eq!(stats.sweep.released_storage_bytes, 2 * SPAN_SIZE_BYTES);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(heap.committed_span_storage_bytes(), 0);
        assert_eq!(
            heap.verify_reference(reference.raw(), None),
            Err(HeapReferenceError::VacantSpan(reference.raw().span_id()))
        );
    }

    #[test]
    /// Repeated majors retain live objects, reclaim once, then make an empty collection a no-op.
    fn repeated_full_major_collections_do_not_redrop_objects() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let node_type = types.try_register::<DropNode>("DropNode").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let mut root = heap
            .try_allocate(
                node_type,
                0,
                0,
                DropNode {
                    next: None,
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();

        assert_eq!(heap.collect_major(&mut root).unwrap().sweep.live_objects, 1);
        assert_eq!(heap.collect_major(&mut root).unwrap().sweep.live_objects, 1);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        let mut no_roots = Vec::<Value>::new();
        assert_eq!(
            heap.collect_major(&mut no_roots)
                .unwrap()
                .sweep
                .reclaimed_objects,
            1
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            heap.collect_major(&mut no_roots)
                .unwrap()
                .sweep
                .scanned_objects,
            0
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    /// Unpublication precedes unsafe drop so a caught destructor panic cannot cause double drop.
    fn destructor_unwind_cannot_republish_or_double_drop_a_slot() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<PanicOnDrop>("PanicOnDrop").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let reference = heap
            .try_allocate(
                object_type,
                0,
                0,
                PanicOnDrop {
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _ = heap.collect_major(&mut no_roots);
        }));
        assert!(unwind.is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            heap.verify_reference(reference.raw(), None),
            Err(HeapReferenceError::UnallocatedSlot(reference.raw()))
        );

        let retry = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(retry.sweep.reclaimed_objects, 0);
        assert_eq!(retry.sweep.spans_released, 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    /// A large drop unwind leaves an unpublished owner that the next major releases without redrop.
    fn large_destructor_unwind_releases_range_on_retry_without_double_drop() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let object_type = types
            .try_register::<LargePanicOnDrop>("LargePanicOnDrop")
            .unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
        heap.try_allocate(
            object_type,
            0,
            0,
            LargePanicOnDrop {
                _bytes: [0; 70_000],
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Old,
        )
        .unwrap();
        let mut no_roots = Vec::<Value>::new();

        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _ = heap.collect_major(&mut no_roots);
        }));
        assert!(unwind.is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(heap.committed_span_storage_bytes(), 2 * SPAN_SIZE_BYTES);

        let retry = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(retry.sweep.reclaimed_objects, 0);
        assert_eq!(retry.sweep.spans_released, 2);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(heap.committed_span_storage_bytes(), 0);
    }
}
