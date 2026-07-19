//! Small-object allocation policy over fixed logical spans.

use crate::{
    BarrierVerificationError, CollectionEpoch, FinalizationQueueStats,
    GC_HEADER_EXTERNAL_BYTES_FLAG, GcAllocationPolicy, GcHeader, GcRef, GcType, GcTypeId,
    GrayQueueStats, HeapReferenceError, KeptObjectStats, LargeAllocationError, LargeReclaim,
    MAX_LOGICAL_OBJECT_COUNT, MAX_LOGICAL_SPANS, MarkError, MarkStats, MinorSweepStats,
    ObjectLayout, RawHeapRef, SPAN_SIZE_BYTES, SmallAllocationError, SmallObjectLayout, SpanId,
    SpanSpace, SpanTable, SpanTableError, SweepError, SweepStats, SweepWorklistStats,
    TemporaryRootStats, Trace, TypeRegistry, WeakOwnerStats, YoungMarkStats,
    eden::EdenPool,
    finalization::PendingFinalizations,
    gray::GrayQueue,
    mark::{mark_strong_roots, mark_young_roots},
    pause::GcPauses,
    persistent::{PersistentRootError, PersistentRootId, PersistentRootStats, PersistentRoots},
    roots::{KeptObjectError, KeptObjects, RootComposition, TemporaryRoots},
    scope::{NoGcBorrowError, NoGcScope, RootError, RunningScope},
    sweep::{SweepWorklist, sweep_full, sweep_young},
    trigger::{CollectionAction, CollectionRequest, GcTrigger},
    tuning::SMALL_SIZE_CLASSES,
    weak::WeakOwners,
};

/// Whether an object enters the young bump path or is allocated directly in old space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationSpace {
    Young,
    Old,
}

/// Reports immutable out-of-line storage owned and dropped by one GC payload.
///
/// The value must remain exact for the allocation's lifetime. External-backed payloads therefore
/// expose immutable backing or route any replacement through a future heap accounting API.
pub trait GcExternalMemory {
    fn external_memory_bytes(&self) -> usize;
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
    ReservedHeaderFlag {
        flags: u16,
    },
    ExternalBytesTooLarge {
        bytes: usize,
        maximum: usize,
    },
    SpanTable(SpanTableError),
    SpanAllocation(SmallAllocationError),
    LargeAllocation(LargeAllocationError),
}

/// A full-major collection fails before sweep or leaves any partial sweep exactly accounted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MajorCollectionError {
    PoolTrim(SpanTableError),
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
    host_external_bytes: usize,
    object_external_bytes: usize,
    collection_epoch: CollectionEpoch,
    gray: GrayQueue,
    sweep_worklist: SweepWorklist,
    temporary_roots: TemporaryRoots,
    persistent_roots: PersistentRoots,
    weak_owners: WeakOwners,
    kept_objects: KeptObjects,
    pending_finalizations: PendingFinalizations,
    eden_pool: EdenPool,
    pauses: GcPauses,
    trigger: GcTrigger,
}

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
            host_external_bytes: 0,
            object_external_bytes: 0,
            collection_epoch: CollectionEpoch::INITIAL,
            gray: GrayQueue::new(max_reference_entries),
            sweep_worklist: SweepWorklist::new(max_sweep_entries),
            temporary_roots: TemporaryRoots::new(max_reference_entries),
            persistent_roots: PersistentRoots::new(max_reference_entries),
            weak_owners: WeakOwners::new(max_reference_entries),
            kept_objects: KeptObjects::new(max_reference_entries),
            pending_finalizations: PendingFinalizations::new(max_reference_entries),
            eden_pool: EdenPool::new(),
            pauses: GcPauses::new(),
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

    #[must_use]
    pub const fn eden_pool_stats(&self) -> crate::EdenPoolStats {
        self.eden_pool.stats()
    }

    /// Records a caller-measured stop-the-world duration from its injected monotonic clock.
    pub fn record_collection_pause(
        &mut self,
        kind: crate::CollectionKind,
        elapsed: core::time::Duration,
    ) {
        self.pauses.record(kind, elapsed);
    }

    #[must_use]
    pub fn pause_stats(&self) -> crate::GcPauseStats {
        self.pauses.stats()
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

    /// Validates a raw logical address and restores its descriptor-proven typed reference.
    pub fn checked_reference<T: Trace + 'static>(
        &self,
        reference: RawHeapRef,
        object_type: GcType<T>,
    ) -> Result<GcRef<T>, HeapReferenceError> {
        if !self.types.matches(object_type) {
            return Err(HeapReferenceError::UnregisteredTypeId {
                reference,
                type_id: object_type.type_id(),
            });
        }
        self.verify_reference(reference, Some(object_type.type_id()))?;
        Ok(GcRef::from_raw(reference))
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

    /// Lends checked payload borrows without creating a temporary-root rollback checkpoint.
    ///
    /// The generative callback cannot accept or return a `Local`; it is intended for references
    /// already retained by a traced runtime value. `NoGcScope` still removes every allocation and
    /// collection operation while descriptor, liveness, and owner validation remain mandatory.
    #[inline(always)]
    pub fn with_no_gc_scope<R>(
        &mut self,
        callback: impl for<'scope, 'no_gc> FnOnce(&mut NoGcScope<'_, 'scope, 'no_gc>) -> R,
    ) -> R {
        let mut no_gc = NoGcScope::new(self);
        callback(&mut no_gc)
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
        let pool_trimmed_bytes = self
            .trim_eden_pool_storage()
            .map_err(MajorCollectionError::PoolTrim)?;
        let mark = self
            .mark_strong(roots)
            .map_err(MajorCollectionError::Mark)?;
        let mut sweep = SweepStats::default();
        let result = sweep_full(
            &mut self.table,
            &self.types,
            &mut self.sweep_worklist,
            self.collection_epoch,
            &mut self.object_external_bytes,
            &mut sweep,
        );
        self.committed_span_storage_bytes = self
            .committed_span_storage_bytes
            .checked_sub(sweep.released_storage_bytes)
            .expect("sweep cannot release more storage than the heap committed");
        sweep.external_bytes = self.external_bytes();
        sweep.spans_released += pool_trimmed_bytes / SPAN_SIZE_BYTES;
        sweep.released_storage_bytes += pool_trimmed_bytes;
        self.populate_sweep_accounting(&mut sweep);
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
            &mut self.eden_pool,
            &mut self.object_external_bytes,
            &mut sweep,
        );
        self.committed_span_storage_bytes = self
            .committed_span_storage_bytes
            .checked_sub(sweep.sweep.released_storage_bytes)
            .expect("minor sweep cannot release more storage than the heap committed");
        sweep.sweep.external_bytes = self.external_bytes();
        self.populate_sweep_accounting(&mut sweep.sweep);
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

    /// Copies cumulative allocation and current live-span accounting into one collection result.
    fn populate_sweep_accounting(&self, sweep: &mut SweepStats) {
        let trigger = self.trigger.stats();
        let (young_live_spans, old_live_spans) = self.table.live_object_span_counts();
        sweep.allocated_young_bytes_total = trigger.young_allocated_bytes;
        sweep.allocated_old_bytes_total = trigger.old_allocated_bytes;
        sweep.young_live_spans = young_live_spans;
        sweep.old_live_spans = old_live_spans;
        sweep.eden_pool_retained_bytes = self.eden_pool.stats().retained_bytes;
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

#[cfg(test)]
mod tests;
