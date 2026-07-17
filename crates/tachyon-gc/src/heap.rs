//! Small-object allocation policy over fixed logical spans.

use crate::{
    GcHeader, GcRef, GcType, GcTypeId, HeapReferenceError, RawHeapRef, SPAN_SIZE_BYTES,
    SmallAllocationError, SmallObjectLayout, SmallObjectLayoutError, SpanId, SpanSpace, SpanTable,
    SpanTableError, Trace, TypeRegistry, tuning::SMALL_SIZE_CLASSES,
};

/// Whether an object enters the young bump path or is allocated directly in old space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationSpace {
    Young,
    Old,
}

/// A host-configured cap for native small-span storage; broader isolate accounting arrives with buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmallHeapLimit {
    max_span_storage_bytes: usize,
}

impl SmallHeapLimit {
    #[must_use]
    pub const fn new(max_span_storage_bytes: usize) -> Self {
        Self {
            max_span_storage_bytes,
        }
    }

    #[must_use]
    pub const fn max_span_storage_bytes(self) -> usize {
        self.max_span_storage_bytes
    }
}

/// A structured small-heap failure; no branch falls back to infallible allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeapAllocationError {
    UnregisteredOrMismatchedType {
        type_id: GcTypeId,
    },
    LargeObjectRequired {
        required: usize,
        alignment: usize,
        largest_small_class: usize,
    },
    SmallSpanLimitExceeded {
        limit: usize,
        committed: usize,
        requested: usize,
    },
    SpanTable(SpanTableError),
    SpanAllocation(SmallAllocationError),
}

/// A single-mutator small-object heap with fixed per-size-class active-span slots.
pub struct SmallHeap {
    types: TypeRegistry,
    table: SpanTable,
    active_eden: [Option<SpanId>; SMALL_SIZE_CLASSES.len()],
    active_old: [Option<SpanId>; SMALL_SIZE_CLASSES.len()],
    limit: SmallHeapLimit,
    committed_span_storage_bytes: usize,
}

impl SmallHeap {
    /// Creates an empty heap without allocating a span or active-size-class side container.
    #[must_use]
    pub const fn new(limit: SmallHeapLimit, types: TypeRegistry) -> Self {
        Self {
            types,
            table: SpanTable::new(),
            active_eden: [None; SMALL_SIZE_CLASSES.len()],
            active_old: [None; SMALL_SIZE_CLASSES.len()],
            limit,
            committed_span_storage_bytes: 0,
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
        let layout = match SmallObjectLayout::for_type::<T>() {
            Ok(layout) => layout,
            Err(SmallObjectLayoutError::AlignmentTooLarge { alignment }) => {
                return Err(large_object_required(
                    core::mem::size_of::<T>() + core::mem::size_of::<crate::GcHeader>(),
                    alignment,
                ));
            }
            Err(SmallObjectLayoutError::SizeTooLarge { size }) => {
                return Err(large_object_required(size, core::mem::align_of::<T>()));
            }
        };
        let Some(class_index) = size_class_index(layout.slot_size()) else {
            return Err(large_object_required(
                usize::from(layout.slot_size()),
                core::mem::align_of::<T>(),
            ));
        };
        self.try_allocate_class(class_index, object_type.type_id(), flags, aux, value, space)
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

    /// Verifies side metadata and rejects non-zero header IDs absent from this heap's registry.
    pub fn verify_reference(
        &self,
        reference: RawHeapRef,
        expected_type: Option<GcTypeId>,
    ) -> Result<GcHeader, HeapReferenceError> {
        let header = self
            .table
            .verify_small_reference(reference, expected_type)?;
        let type_id = header
            .type_id()
            .expect("table verification already rejected a zero type ID");
        if self.types.descriptor(type_id).is_none() {
            return Err(HeapReferenceError::UnregisteredTypeId { reference, type_id });
        }
        Ok(header)
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
        let requested = self
            .committed_span_storage_bytes
            .saturating_add(SPAN_SIZE_BYTES);
        if requested > self.limit.max_span_storage_bytes() {
            return Err(HeapAllocationError::SmallSpanLimitExceeded {
                limit: self.limit.max_span_storage_bytes(),
                committed: self.committed_span_storage_bytes,
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
        self.committed_span_storage_bytes = requested;
        match space {
            AllocationSpace::Young => self.active_eden[class_index] = Some(span_id),
            AllocationSpace::Old => self.active_old[class_index] = Some(span_id),
        }
        self.table
            .try_allocate_in_span(span_id, type_id, flags, aux, value)
            .map_err(HeapAllocationError::SpanAllocation)
    }
}

#[inline(always)]
fn size_class_index(required: u16) -> Option<usize> {
    let index = SMALL_SIZE_CLASSES.partition_point(|&class| class < required);
    (index < SMALL_SIZE_CLASSES.len()).then_some(index)
}

fn large_object_required(required: usize, alignment: usize) -> HeapAllocationError {
    HeapAllocationError::LargeObjectRequired {
        required,
        alignment,
        largest_small_class: usize::from(
            *SMALL_SIZE_CLASSES
                .last()
                .expect("small size-class table is non-empty"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{AllocationSpace, HeapAllocationError, SmallHeap, SmallHeapLimit};
    use crate::{SPAN_SIZE_BYTES, Trace, Tracer, TypeRegistry};
    use tachyon_value::Value;

    struct OtherPayload;

    impl Trace for OtherPayload {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    #[test]
    fn first_allocation_uses_slow_path_then_reuses_the_active_eden_span() {
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<Value>("Value").unwrap();
        let mut heap = SmallHeap::new(SmallHeapLimit::new(SPAN_SIZE_BYTES), types);
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
        let mut heap = SmallHeap::new(SmallHeapLimit::new(SPAN_SIZE_BYTES), types);
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
            Err(HeapAllocationError::SmallSpanLimitExceeded {
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
        let mut heap = SmallHeap::new(SmallHeapLimit::new(SPAN_SIZE_BYTES), conflicting_registry);

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
}
