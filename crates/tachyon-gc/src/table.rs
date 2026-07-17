//! Stable logical span indexing over independently allocated native storage.

use crate::{
    CollectionEpoch, GcHeader, GcRef, GcTypeId, MAX_LOGICAL_SPANS, RawHeapRef, SizeClass,
    SmallObjectLayout, SmallObjectLayoutError, SmallSpanMetadata, SpanId, SpanReuseGeneration,
    SpanSpace, SpanStorage, SpanStorageAllocationError, Trace,
    tuning::{
        CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_FREE_RANGE_CAPACITY,
        INITIAL_SPAN_TABLE_CAPACITY,
    },
};

const MAX_FREE_SPAN_RANGES: usize = MAX_LOGICAL_SPANS.div_ceil(2);

/// A recoverable failure at the span-table allocation or validation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanTableError {
    AddressSpaceExhausted,
    TableAllocationFailed,
    FreeRangeAllocationFailed,
    StorageAllocationFailed,
    UnknownSpan(SpanId),
    VacantSpan(SpanId),
    LiveSpan(SpanId),
}

/// A rejected object allocation that leaves every published allocation bit unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmallAllocationError {
    UnknownSpan(SpanId),
    VacantSpan(SpanId),
    SurvivorIsNotAllocatable(SpanId),
    SpanFull(SpanId),
    InvalidInlineLayout(SmallObjectLayoutError),
    SizeClassTooSmall { required: u16, actual: u16 },
}

/// A failed exact-reference check at the collector/debug boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeapReferenceError {
    UnknownSpan(SpanId),
    VacantSpan(SpanId),
    InvalidSlotBoundary(RawHeapRef),
    UnallocatedSlot(RawHeapRef),
    InvalidTypeId(RawHeapRef),
    UnregisteredTypeId {
        reference: RawHeapRef,
        type_id: GcTypeId,
    },
    TypeMismatch {
        expected: GcTypeId,
        actual: GcTypeId,
    },
}

impl From<SpanStorageAllocationError> for SpanTableError {
    fn from(_: SpanStorageAllocationError) -> Self {
        Self::StorageAllocationFailed
    }
}

struct SmallSpan {
    storage: SpanStorage,
    metadata: SmallSpanMetadata,
}

struct SpanEntry {
    generation: SpanReuseGeneration,
    span: Option<SmallSpan>,
}

/// A coalesced inclusive range of vacant logical span IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FreeSpanRange {
    start: u16,
    end: u16,
}

/// An isolate-local mapping whose entry indices remain stable across metadata-vector growth.
#[derive(Default)]
pub struct SpanTable {
    entries: Vec<SpanEntry>,
    free_ranges: Vec<FreeSpanRange>,
    live_spans: usize,
}

impl SpanTable {
    /// Creates an empty table; the first slow-path insertion performs the educated reservation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            free_ranges: Vec::new(),
            live_spans: 0,
        }
    }

    /// Allocates one small-object span and installs it at a reused or new stable table index.
    pub fn try_allocate_small(
        &mut self,
        size_class: SizeClass,
        space: SpanSpace,
    ) -> Result<SpanId, SpanTableError> {
        let storage = SpanStorage::try_new()?;
        if let Some(span_id) = self.take_free_span() {
            self.install_reused(span_id, storage, size_class, space);
            return Ok(span_id);
        }

        self.install_new(storage, size_class, space)
    }

    /// Releases native storage while retaining the table index and its incremented reuse generation.
    pub fn release(&mut self, span_id: SpanId) -> Result<(), SpanTableError> {
        let index = span_id.index() as usize;
        let Some(entry) = self.entries.get(index) else {
            return Err(SpanTableError::UnknownSpan(span_id));
        };
        if entry.span.is_none() {
            return Err(SpanTableError::VacantSpan(span_id));
        }
        if entry
            .span
            .as_ref()
            .is_some_and(|span| span.metadata.allocated_slots() != 0)
        {
            return Err(SpanTableError::LiveSpan(span_id));
        }
        self.reserve_free_range_if_needed(span_id.index())?;

        let entry = &mut self.entries[index];
        entry.span = None;
        entry.generation = entry.generation.next();
        self.insert_free_span(span_id.index());
        self.live_spans -= 1;
        Ok(())
    }

    /// Resolves through the current table entry every time instead of caching a native object pointer.
    #[must_use]
    pub fn base_address(&self, span_id: SpanId) -> Option<*const u8> {
        self.entries
            .get(span_id.index() as usize)?
            .span
            .as_ref()
            .map(|span| span.storage.base_address())
    }

    /// Returns immutable side metadata for collector and verifier operations.
    #[must_use]
    pub fn metadata(&self, span_id: SpanId) -> Option<&SmallSpanMetadata> {
        self.entries
            .get(span_id.index() as usize)?
            .span
            .as_ref()
            .map(|span| &span.metadata)
    }

    /// Returns mutable side metadata while preserving exclusive isolate ownership.
    #[must_use]
    pub fn metadata_mut(&mut self, span_id: SpanId) -> Option<&mut SmallSpanMetadata> {
        self.entries
            .get_mut(span_id.index() as usize)?
            .span
            .as_mut()
            .map(|span| &mut span.metadata)
    }

    /// Returns the count of currently backed entries.
    #[must_use]
    pub const fn live_spans(&self) -> usize {
        self.live_spans
    }

    /// Returns the historical table length; released trailing IDs deliberately do not shrink it.
    #[must_use]
    pub fn historical_span_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns retained entry capacity for later capacity instrumentation and accounting.
    #[must_use]
    pub fn retained_entry_capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// Checks the active-span fast path without consuming the value that a slow path may need.
    #[must_use]
    #[inline(always)]
    pub fn can_allocate_in_span(&self, span_id: SpanId) -> bool {
        self.entries
            .get(span_id.index() as usize)
            .and_then(|entry| entry.span.as_ref())
            .is_some_and(|span| span.metadata.has_allocation_capacity())
    }

    /// Initializes one typed object in a selected span without invoking a table/GC slow path.
    #[inline(always)]
    pub(crate) fn try_allocate_in_span<T: Trace>(
        &mut self,
        span_id: SpanId,
        type_id: GcTypeId,
        flags: u16,
        aux: u32,
        value: T,
    ) -> Result<GcRef<T>, SmallAllocationError> {
        let layout = SmallObjectLayout::for_type::<T>()
            .map_err(SmallAllocationError::InvalidInlineLayout)?;
        let span = self
            .entries
            .get_mut(span_id.index() as usize)
            .ok_or(SmallAllocationError::UnknownSpan(span_id))?
            .span
            .as_mut()
            .ok_or(SmallAllocationError::VacantSpan(span_id))?;
        let actual = span.metadata.size_class().slot_size();
        if layout.slot_size() > actual {
            return Err(SmallAllocationError::SizeClassTooSmall {
                required: layout.slot_size(),
                actual,
            });
        }
        let slot = span.take_allocation_slot(span_id)?;
        let offset = span
            .metadata
            .size_class()
            .offset_for_slot(slot)
            .expect("allocation candidates belong to the span size class");
        span.storage
            .initialize(offset, GcHeader::new(type_id, flags, aux), value)
            .expect("size-class validation guarantees an aligned in-span write");
        span.metadata.commit_allocation(slot);
        Ok(GcRef::from_raw(RawHeapRef::from_parts(span_id, offset)))
    }

    /// Verifies a live small object without relying on native page faults or cached pointers.
    pub fn verify_small_reference(
        &self,
        reference: RawHeapRef,
        expected_type: Option<GcTypeId>,
    ) -> Result<GcHeader, HeapReferenceError> {
        let span_id = reference.span_id();
        let entry = self
            .entries
            .get(span_id.index() as usize)
            .ok_or(HeapReferenceError::UnknownSpan(span_id))?;
        let span = entry
            .span
            .as_ref()
            .ok_or(HeapReferenceError::VacantSpan(span_id))?;
        let slot = span
            .metadata
            .size_class()
            .slot_for_offset(reference.span_offset())
            .ok_or(HeapReferenceError::InvalidSlotBoundary(reference))?;
        if !span.metadata.allocations().is_allocated(slot) {
            return Err(HeapReferenceError::UnallocatedSlot(reference));
        }
        let header = span
            .storage
            .header(reference.span_offset())
            .expect("validated small-object slots always contain a complete header");
        let actual = header
            .type_id()
            .ok_or(HeapReferenceError::InvalidTypeId(reference))?;
        if let Some(expected) = expected_type
            && actual != expected
        {
            return Err(HeapReferenceError::TypeMismatch { expected, actual });
        }
        Ok(header)
    }

    /// Reclaims an object after its descriptor drop callback has completed.
    ///
    /// The collector owns sequencing: calling this early leaks Rust resources when the slot is
    /// overwritten, but cannot expose a safe typed borrow because resolution remains scope-owned.
    pub fn reclaim_small_after_drop(&mut self, reference: RawHeapRef) -> bool {
        let Some(span) = self
            .entries
            .get_mut(reference.span_id().index() as usize)
            .and_then(|entry| entry.span.as_mut())
        else {
            return false;
        };
        let Some(slot) = span
            .metadata
            .size_class()
            .slot_for_offset(reference.span_offset())
        else {
            return false;
        };
        if !span.metadata.reclaim_allocation(slot) {
            return false;
        }
        if span.metadata.space() == SpanSpace::Old {
            let previous = span.metadata.free_list_head();
            span.storage
                .write_free_next(reference.span_offset(), previous);
            span.metadata.set_free_list_head(Some(slot));
        }
        true
    }

    /// Advances the epoch, physically resetting every live span bitmap on the forced-wrap path.
    pub fn advance_collection_epoch(&mut self, current: CollectionEpoch) -> CollectionEpoch {
        match current.next() {
            Ok(next) => next,
            Err(_) => {
                for entry in &mut self.entries {
                    if let Some(span) = &mut entry.span {
                        span.metadata.marks_mut().reset_for_epoch_overflow();
                    }
                }
                CollectionEpoch::INITIAL
            }
        }
    }

    fn install_new(
        &mut self,
        storage: SpanStorage,
        size_class: SizeClass,
        space: SpanSpace,
    ) -> Result<SpanId, SpanTableError> {
        if self.entries.len() == MAX_LOGICAL_SPANS {
            return Err(SpanTableError::AddressSpaceExhausted);
        }
        reserve_for_push(
            &mut self.entries,
            INITIAL_SPAN_TABLE_CAPACITY,
            MAX_LOGICAL_SPANS,
            SpanTableError::TableAllocationFailed,
        )?;
        let index = u16::try_from(self.entries.len()).expect("logical span limit checked above");
        let generation = SpanReuseGeneration::INITIAL;
        self.entries.push(SpanEntry {
            generation,
            span: Some(SmallSpan {
                storage,
                metadata: SmallSpanMetadata::new(size_class, space, generation),
            }),
        });
        self.live_spans += 1;
        Ok(SpanId::new(index))
    }

    fn install_reused(
        &mut self,
        span_id: SpanId,
        storage: SpanStorage,
        size_class: SizeClass,
        space: SpanSpace,
    ) {
        let entry = &mut self.entries[span_id.index() as usize];
        debug_assert!(entry.span.is_none());
        entry.span = Some(SmallSpan {
            storage,
            metadata: SmallSpanMetadata::new(size_class, space, entry.generation),
        });
        self.live_spans += 1;
    }

    fn take_free_span(&mut self) -> Option<SpanId> {
        let range = self.free_ranges.last_mut()?;
        let index = range.end;
        if range.start == range.end {
            self.free_ranges.pop();
        } else {
            range.end -= 1;
        }
        Some(SpanId::new(index))
    }

    fn reserve_free_range_if_needed(&mut self, index: u16) -> Result<(), SpanTableError> {
        if self.adjacent_free_range(index).is_some() {
            return Ok(());
        }
        reserve_for_push(
            &mut self.free_ranges,
            INITIAL_FREE_RANGE_CAPACITY,
            MAX_FREE_SPAN_RANGES,
            SpanTableError::FreeRangeAllocationFailed,
        )
    }

    fn adjacent_free_range(&self, index: u16) -> Option<usize> {
        self.free_ranges.iter().position(|range| {
            range.start as u32 <= index as u32 + 1 && index as u32 <= range.end as u32 + 1
        })
    }

    /// Inserts one newly vacant ID and coalesces both neighboring ranges without allocating.
    fn insert_free_span(&mut self, index: u16) {
        let position = self
            .free_ranges
            .partition_point(|range| range.start < index);
        let joins_left =
            position > 0 && self.free_ranges[position - 1].end as u32 + 1 == index as u32;
        let joins_right = position < self.free_ranges.len()
            && index as u32 + 1 == self.free_ranges[position].start as u32;

        match (joins_left, joins_right) {
            (true, true) => {
                let right_end = self.free_ranges[position].end;
                self.free_ranges[position - 1].end = right_end;
                self.free_ranges.remove(position);
            }
            (true, false) => self.free_ranges[position - 1].end = index,
            (false, true) => self.free_ranges[position].start = index,
            (false, false) => self.free_ranges.insert(
                position,
                FreeSpanRange {
                    start: index,
                    end: index,
                },
            ),
        }
    }
}

impl SmallSpan {
    /// Selects a cohort-legal slot while keeping free-list bytes and metadata synchronized.
    #[inline(always)]
    fn take_allocation_slot(
        &mut self,
        span_id: SpanId,
    ) -> Result<crate::SlotIndex, SmallAllocationError> {
        match self.metadata.space() {
            SpanSpace::Survivor { .. } => {
                Err(SmallAllocationError::SurvivorIsNotAllocatable(span_id))
            }
            SpanSpace::Eden => self
                .metadata
                .take_bump_slot()
                .ok_or(SmallAllocationError::SpanFull(span_id)),
            SpanSpace::Old => {
                if let Some(slot) = self.metadata.free_list_head() {
                    let offset = self
                        .metadata
                        .size_class()
                        .offset_for_slot(slot)
                        .expect("free-list slot belongs to this size class");
                    let next = self.storage.read_free_next(offset);
                    self.metadata.set_free_list_head(next);
                    Ok(slot)
                } else {
                    self.metadata
                        .take_bump_slot()
                        .ok_or(SmallAllocationError::SpanFull(span_id))
                }
            }
        }
    }
}

/// Reserves a centralized 1.5x capacity step before a push can mutate container state.
fn reserve_for_push<T>(
    values: &mut Vec<T>,
    initial_capacity: usize,
    maximum_capacity: usize,
    error: SpanTableError,
) -> Result<(), SpanTableError> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let target = if values.capacity() == 0 {
        initial_capacity
    } else {
        values
            .capacity()
            .saturating_mul(CAPACITY_GROWTH_NUMERATOR)
            .div_ceil(CAPACITY_GROWTH_DENOMINATOR)
    };
    let target = target.max(values.len() + 1).min(maximum_capacity);
    let additional = target - values.len();
    values.try_reserve_exact(additional).map_err(|_| error)
}

#[cfg(test)]
mod tests {
    use super::{SpanTable, SpanTableError};
    use crate::{
        CollectionEpoch, GcTypeId, HeapReferenceError, RawHeapRef, SizeClass, SlotIndex,
        SmallAllocationError, SpanId, SpanOffset, SpanReuseGeneration, SpanSpace,
    };
    use tachyon_value::{Immediate, Value};

    fn size_class() -> SizeClass {
        SizeClass::new(16).expect("minimum size class")
    }

    #[test]
    /// Forces metadata-vector growth and proves independently allocated span storage stays stable.
    fn table_grows_on_demand_and_resolves_stable_independent_storage() {
        let mut table = SpanTable::new();
        assert_eq!(table.historical_span_count(), 0);
        assert_eq!(table.retained_entry_capacity(), 0);

        let first = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let first_address = table.base_address(first).unwrap();
        let mut last = first;
        for _ in 1..32 {
            last = table
                .try_allocate_small(size_class(), SpanSpace::Old)
                .unwrap();
        }

        assert_eq!(first, SpanId::new(0));
        assert_eq!(last, SpanId::new(31));
        assert_eq!(table.live_spans(), 32);
        assert_eq!(table.historical_span_count(), 32);
        assert_eq!(table.base_address(first), Some(first_address));
        assert_ne!(table.base_address(last), Some(first_address));
    }

    #[test]
    /// Releases coalesced ranges, reuses a stable ID, and exposes its incremented generation.
    fn table_reuses_free_ranges_without_shrinking_historical_indices() {
        let mut table = SpanTable::new();
        let first = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let second = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let third = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();

        table.release(first).unwrap();
        table.release(second).unwrap();
        table.release(third).unwrap();
        assert_eq!(table.live_spans(), 0);
        assert_eq!(table.historical_span_count(), 3);
        assert_eq!(table.base_address(second), None);
        assert_eq!(
            table.release(second),
            Err(SpanTableError::VacantSpan(second))
        );

        let reused = table
            .try_allocate_small(size_class(), SpanSpace::Old)
            .unwrap();
        assert_eq!(reused, third);
        assert_eq!(
            table.metadata(reused).unwrap().reuse_generation(),
            SpanReuseGeneration::INITIAL.next()
        );
        assert_eq!(table.historical_span_count(), 3);
    }

    #[test]
    fn unknown_span_ids_are_rejected_without_mutating_the_table() {
        let mut table = SpanTable::new();
        let unknown = SpanId::new(7);
        assert_eq!(
            table.release(unknown),
            Err(SpanTableError::UnknownSpan(unknown))
        );
        assert_eq!(table.live_spans(), 0);
    }

    #[test]
    /// Forces epoch wrap and proves all live span bitmaps are reset before epoch one is reused.
    fn epoch_overflow_resets_every_live_span_bitmap() {
        let mut table = SpanTable::new();
        let first = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let second = table
            .try_allocate_small(size_class(), SpanSpace::Old)
            .unwrap();
        let maximum = CollectionEpoch::new(u32::MAX).unwrap();
        let slot = SlotIndex::new(0).unwrap();
        assert!(
            table
                .metadata_mut(first)
                .unwrap()
                .marks_mut()
                .mark(slot, maximum)
        );
        assert!(
            table
                .metadata_mut(second)
                .unwrap()
                .marks_mut()
                .mark(slot, maximum)
        );

        let next = table.advance_collection_epoch(maximum);

        assert_eq!(next, CollectionEpoch::INITIAL);
        assert!(
            !table
                .metadata(first)
                .unwrap()
                .marks()
                .is_marked(slot, maximum)
        );
        assert!(
            !table
                .metadata(second)
                .unwrap()
                .marks()
                .is_marked(slot, maximum)
        );
        assert!(
            table
                .metadata_mut(first)
                .unwrap()
                .marks_mut()
                .mark(slot, next)
        );
    }

    #[test]
    /// Publishes only initialized objects and covers verifier boundary/type/allocation failures.
    fn typed_small_allocation_and_reference_verification_agree() {
        let mut table = SpanTable::new();
        let span = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let type_id = GcTypeId::new(9).unwrap();
        let reference = table
            .try_allocate_in_span(
                span,
                type_id,
                0x55aa,
                17,
                Value::from_immediate(Immediate::Null),
            )
            .unwrap();

        let header = table
            .verify_small_reference(reference.raw(), Some(type_id))
            .unwrap();
        assert_eq!(header.type_id(), Some(type_id));
        assert_eq!(header.flags(), 0x55aa);
        assert_eq!(header.aux(), 17);
        assert_eq!(table.metadata(span).unwrap().allocated_slots(), 1);
        assert_eq!(table.release(span), Err(SpanTableError::LiveSpan(span)));

        let wrong_type = GcTypeId::new(10).unwrap();
        assert_eq!(
            table.verify_small_reference(reference.raw(), Some(wrong_type)),
            Err(HeapReferenceError::TypeMismatch {
                expected: wrong_type,
                actual: type_id,
            })
        );
        let unallocated = RawHeapRef::from_parts(span, SpanOffset::new(32).unwrap());
        assert_eq!(
            table.verify_small_reference(unallocated, None),
            Err(HeapReferenceError::UnallocatedSlot(unallocated))
        );
        let misaligned = RawHeapRef::from_parts(span, SpanOffset::new(17).unwrap());
        assert_eq!(
            table.verify_small_reference(misaligned, None),
            Err(HeapReferenceError::InvalidSlotBoundary(misaligned))
        );
    }

    #[test]
    /// Proves Survivor rejects allocation and Old reuses reclaimed slots before bumping.
    fn cohort_allocation_paths_enforce_survivor_and_old_free_list_rules() {
        let mut table = SpanTable::new();
        let type_id = GcTypeId::new(1).unwrap();
        let survivor = table
            .try_allocate_small(size_class(), SpanSpace::Survivor { age: 1 })
            .unwrap();
        assert_eq!(
            table.try_allocate_in_span(survivor, type_id, 0, 0, Value::from_i32(1)),
            Err(SmallAllocationError::SurvivorIsNotAllocatable(survivor))
        );

        let old = table
            .try_allocate_small(size_class(), SpanSpace::Old)
            .unwrap();
        let first = table
            .try_allocate_in_span(old, type_id, 0, 0, Value::from_i32(1))
            .unwrap();
        let second = table
            .try_allocate_in_span(old, type_id, 0, 0, Value::from_i32(2))
            .unwrap();
        assert!(table.reclaim_small_after_drop(first.raw()));
        let reused = table
            .try_allocate_in_span(old, type_id, 0, 0, Value::from_i32(3))
            .unwrap();
        assert_eq!(reused.raw(), first.raw());
        assert_ne!(reused.raw(), second.raw());
        assert_eq!(table.metadata(old).unwrap().allocated_slots(), 2);
    }

    #[test]
    fn selected_span_rejects_payloads_larger_than_its_size_class() {
        #[derive(Debug, Eq, PartialEq)]
        struct Payload([u8; 16]);

        impl crate::Trace for Payload {
            fn trace(&mut self, _: &mut dyn crate::Tracer) {}
        }

        let mut table = SpanTable::new();
        let span = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        assert_eq!(
            table.try_allocate_in_span(span, GcTypeId::new(1).unwrap(), 0, 0, Payload([0; 16])),
            Err(SmallAllocationError::SizeClassTooSmall {
                required: 32,
                actual: 16,
            })
        );
        assert_eq!(core::mem::size_of::<Payload>(), 16);
        assert_eq!(table.metadata(span).unwrap().allocated_slots(), 0);
    }
}
