//! Small-object allocation policy over fixed logical spans.

use crate::{
    CollectionEpoch, GcHeader, GcRef, GcType, GcTypeId, GrayQueueStats, HeapReferenceError,
    LargeAllocationError, LargeReclaim, MarkError, MarkStats, ObjectLayout, RawHeapRef,
    SPAN_SIZE_BYTES, SmallAllocationError, SmallObjectLayout, SpanId, SpanSpace, SpanTable,
    SpanTableError, Trace, TypeRegistry, gray::GrayQueue, mark::mark_strong_roots,
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
}

impl Heap {
    /// Creates an empty heap without allocating a span or active-size-class side container.
    #[must_use]
    pub const fn new(limit: HeapLimit, types: TypeRegistry) -> Self {
        Self {
            types,
            table: SpanTable::new(),
            active_eden: [None; SMALL_SIZE_CLASSES.len()],
            active_old: [None; SMALL_SIZE_CLASSES.len()],
            limit,
            committed_span_storage_bytes: 0,
            external_bytes: 0,
            collection_epoch: CollectionEpoch::INITIAL,
            gray: GrayQueue::new(limit.max_heap_bytes() / crate::MINIMUM_SLOT_SIZE_BYTES),
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
        mark_strong_roots(
            &mut self.table,
            &self.types,
            &mut self.gray,
            self.collection_epoch,
            roots,
        )
    }

    /// Returns retained gray high-water evidence for tuning and quota tests.
    #[must_use]
    pub fn gray_queue_stats(&self) -> GrayQueueStats {
        self.gray.stats()
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
}
