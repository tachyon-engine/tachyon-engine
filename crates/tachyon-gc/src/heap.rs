//! Small-object allocation policy over fixed logical spans.

use crate::{
    CollectionEpoch, GcHeader, GcRef, GcType, GcTypeId, GrayQueueStats, HeapReferenceError,
    LargeAllocationError, LargeReclaim, MAX_LOGICAL_OBJECT_COUNT, MAX_LOGICAL_SPANS, MarkError,
    MarkStats, ObjectLayout, RawHeapRef, SPAN_SIZE_BYTES, SmallAllocationError, SmallObjectLayout,
    SpanId, SpanSpace, SpanTable, SpanTableError, SweepError, SweepStats, SweepWorklistStats,
    TemporaryRootStats, Trace, TypeRegistry,
    gray::GrayQueue,
    mark::mark_strong_roots,
    persistent::{PersistentRootError, PersistentRootId, PersistentRootStats, PersistentRoots},
    roots::{RootComposition, TemporaryRoots},
    scope::{NoGcBorrowError, RootError, RunningScope},
    sweep::{SweepWorklist, sweep_full},
    tuning::SMALL_SIZE_CLASSES,
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

/// Combined fixed-point and sweep evidence for one stop-the-world full major collection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MajorCollectionStats {
    pub mark: MarkStats,
    pub sweep: SweepStats,
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
}

impl Heap {
    /// Creates an empty heap without allocating a span or active-size-class side container.
    #[must_use]
    pub const fn new(limit: HeapLimit, types: TypeRegistry) -> Self {
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
        if !self.types.matches(object_type) {
            return Err(HeapAllocationError::UnregisteredOrMismatchedType {
                type_id: object_type.type_id(),
            });
        }
        if let Ok(layout) = SmallObjectLayout::for_type::<T>()
            && let Some(class_index) = size_class_index(layout.slot_size())
        {
            return self.try_allocate_class(
                class_index,
                object_type.type_id(),
                flags,
                aux,
                value,
                space,
            );
        }
        self.allocate_large(object_type.type_id(), flags, aux, value)
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
        let mut roots =
            RootComposition::new(roots, &mut self.temporary_roots, &mut self.persistent_roots);
        mark_strong_roots(
            &mut self.table,
            &self.types,
            &mut self.gray,
            self.collection_epoch,
            &mut roots,
        )
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
        Ok(MajorCollectionStats { mark, sweep })
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
        let layout = ObjectLayout::for_type::<T>().map_err(|_| {
            HeapAllocationError::LargeAllocation(LargeAllocationError::AddressSpaceExhausted)
        })?;
        let logical_bytes = crate::MINIMUM_SLOT_SIZE_BYTES
            .checked_add(layout.allocation_size())
            .ok_or(HeapAllocationError::LargeAllocation(
                LargeAllocationError::AddressSpaceExhausted,
            ))?;
        let requested = logical_bytes
            .div_ceil(SPAN_SIZE_BYTES)
            .checked_mul(SPAN_SIZE_BYTES)
            .ok_or(HeapAllocationError::LargeAllocation(
                LargeAllocationError::AddressSpaceExhausted,
            ))?;
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
        GcRef, HeapReferenceError, RawHeapRef, SPAN_SIZE_BYTES, Trace, Tracer, TypeRegistry,
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

    struct Leaf;

    struct Fanout {
        edges: [Option<GcRef<Leaf>>; 300],
    }

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

    impl Trace for Leaf {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for Fanout {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.edges.trace(tracer);
        }
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
