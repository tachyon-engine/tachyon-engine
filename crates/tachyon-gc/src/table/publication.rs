//! Stable span backing publication, logical-range reuse, and reclamation.

use super::{
    LargeContinuation, LargeSpan, MetadataBox, SmallSpan, SpanEntry, SpanKind, SpanTable,
    SpanTableError,
};
use crate::{
    GcHeader, GcRef, GcTypeId, LargeSpanMetadata, MAX_LOGICAL_SPANS, ObjectLayout, RawHeapRef,
    SizeClass, SmallSpanMetadata, SpanId, SpanReuseGeneration, SpanSpace, SpanStorage, Trace,
    tuning::{
        CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_FREE_RANGE_CAPACITY,
        INITIAL_SPAN_TABLE_CAPACITY,
    },
};

const MAX_FREE_SPAN_RANGES: usize = MAX_LOGICAL_SPANS.div_ceil(2);

/// A large-object range or storage failure before an owner entry becomes visible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LargeAllocationError {
    AddressSpaceExhausted,
    TableAllocationFailed,
    StorageAllocationFailed,
    PayloadAlignmentTooLarge { alignment: usize },
}

/// Accounting returned after an already-dropped large owner range is released.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LargeReclaim {
    span_count: u32,
    storage_bytes: usize,
    object_bytes: usize,
}

impl LargeReclaim {
    #[must_use]
    pub const fn span_count(self) -> u32 {
        self.span_count
    }

    #[must_use]
    pub const fn storage_bytes(self) -> usize {
        self.storage_bytes
    }

    #[must_use]
    pub const fn object_bytes(self) -> usize {
        self.object_bytes
    }
}

impl SmallSpan {
    fn try_new(
        storage: SpanStorage,
        size_class: SizeClass,
        space: SpanSpace,
        generation: SpanReuseGeneration,
    ) -> Result<Self, SpanTableError> {
        Ok(Self {
            storage,
            metadata: MetadataBox::try_new(SmallSpanMetadata::new(size_class, space, generation))?,
        })
    }
}

/// A coalesced inclusive range of vacant logical span IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FreeSpanRange {
    start: u16,
    end: u16,
}

#[derive(Clone, Copy)]
enum SpanRangePlacement {
    Reused { owner: SpanId },
    Append { owner: SpanId },
}

impl SpanRangePlacement {
    const fn owner(self) -> SpanId {
        match self {
            Self::Reused { owner } | Self::Append { owner } => owner,
        }
    }
}

impl SpanTable {
    /// Allocates one small-object span and installs it at a reused or new stable table index.
    pub fn try_allocate_small(
        &mut self,
        size_class: SizeClass,
        space: SpanSpace,
    ) -> Result<SpanId, SpanTableError> {
        let storage = SpanStorage::try_new()?;
        let span = SmallSpan::try_new(storage, size_class, space, SpanReuseGeneration::INITIAL)?;
        let span_id = if let Some(span_id) = self.take_free_span() {
            self.install_reused(span_id, span);
            span_id
        } else {
            self.install_new(span)?
        };
        if space != SpanSpace::Old {
            self.link_young_span(span_id);
            self.active_young_spans = self
                .active_young_spans
                .checked_add(1)
                .expect("active young spans cannot exceed the logical span table");
        }
        Ok(span_id)
    }

    /// Releases native storage while retaining the table index and its incremented reuse generation.
    pub fn release(&mut self, span_id: SpanId) -> Result<(), SpanTableError> {
        let index = span_id.index() as usize;
        let Some(entry) = self.entries.get(index) else {
            return Err(SpanTableError::UnknownSpan(span_id));
        };
        if entry.kind.is_none() {
            return Err(SpanTableError::VacantSpan(span_id));
        }
        match entry.kind.as_ref().expect("checked occupied entry") {
            SpanKind::Small(span) if span.metadata.allocated_slots() != 0 => {
                return Err(SpanTableError::LiveSpan(span_id));
            }
            SpanKind::Small(_) => {}
            SpanKind::LargeOwner(_) => return Err(SpanTableError::LargeOwnerSpan(span_id)),
            SpanKind::LargeContinuation(continuation) => {
                return Err(SpanTableError::LargeContinuationSpan {
                    span: span_id,
                    owner: continuation.owner,
                });
            }
        }
        self.reserve_free_range_if_needed(span_id.index())?;

        let was_active_young = self.entries[index].kind.as_ref().is_some_and(|kind| {
            matches!(
                kind,
                SpanKind::Small(span)
                    if matches!(span.metadata.space(), SpanSpace::Eden | SpanSpace::Survivor { .. })
            ) && !self.entries[index].in_eden_pool
        });
        let entry = &mut self.entries[index];
        entry.kind = None;
        entry.generation = entry.generation.next();
        entry.in_eden_pool = false;
        self.insert_free_span(span_id.index());
        self.live_spans -= 1;
        self.active_young_spans = self
            .active_young_spans
            .checked_sub(usize::from(was_active_young))
            .expect("release cannot underflow active young span accounting");
        Ok(())
    }

    /// Allocates one independently backed large object and publishes its contiguous logical range.
    pub(crate) fn try_allocate_large<T: Trace>(
        &mut self,
        type_id: GcTypeId,
        flags: u16,
        aux: u32,
        value: T,
    ) -> Result<(GcRef<T>, usize), LargeAllocationError> {
        let layout = ObjectLayout::for_type::<T>()
            .map_err(|_| LargeAllocationError::AddressSpaceExhausted)?;
        if layout.alignment() > crate::MINIMUM_SLOT_SIZE_BYTES {
            return Err(LargeAllocationError::PayloadAlignmentTooLarge {
                alignment: layout.alignment(),
            });
        }
        let logical_bytes = crate::MINIMUM_SLOT_SIZE_BYTES
            .checked_add(layout.allocation_size())
            .ok_or(LargeAllocationError::AddressSpaceExhausted)?;
        let span_count = logical_bytes.div_ceil(crate::SPAN_SIZE_BYTES);
        if span_count == 0 || span_count > MAX_LOGICAL_SPANS {
            return Err(LargeAllocationError::AddressSpaceExhausted);
        }
        let mut storage = SpanStorage::try_new_span_count(span_count)
            .map_err(|_| LargeAllocationError::StorageAllocationFailed)?;
        let placement = self.reserve_span_range(span_count)?;
        let owner = placement.owner();
        let offset = crate::SpanOffset::new(crate::MINIMUM_SLOT_SIZE_BYTES as u16)
            .expect("large owners reserve the non-zero first slot offset");
        storage
            .initialize(offset, GcHeader::new(type_id, flags, aux), value)
            .expect("large range sizing and alignment were validated before publication");
        self.install_large(placement, storage, layout.allocation_size(), span_count);
        Ok((
            GcRef::from_raw(RawHeapRef::from_parts(owner, offset)),
            span_count * crate::SPAN_SIZE_BYTES,
        ))
    }

    /// Unpublishes one large payload before drop while retaining its complete backing range.
    pub(crate) fn unpublish_large_for_drop(
        &mut self,
        reference: RawHeapRef,
    ) -> Result<(), SpanTableError> {
        let owner = reference.span_id();
        let Some(SpanKind::LargeOwner(span)) = self
            .entries
            .get_mut(owner.index() as usize)
            .and_then(|entry| entry.kind.as_mut())
        else {
            return Err(SpanTableError::VacantSpan(owner));
        };
        if !span.metadata.is_allocated() {
            return Err(SpanTableError::VacantSpan(owner));
        }
        span.metadata.reclaim();
        Ok(())
    }

    /// Releases an unpublished large owner after its descriptor callback has returned.
    pub(crate) fn release_unpublished_large(
        &mut self,
        reference: RawHeapRef,
    ) -> Result<LargeReclaim, SpanTableError> {
        let owner = reference.span_id();
        let (span_count, object_bytes) = match self
            .entries
            .get(owner.index() as usize)
            .and_then(|entry| entry.kind.as_ref())
        {
            Some(SpanKind::LargeOwner(span)) if !span.metadata.is_allocated() => (
                span.metadata.span_count() as usize,
                span.metadata.object_bytes(),
            ),
            Some(SpanKind::LargeContinuation(continuation)) => {
                return Err(SpanTableError::LargeContinuationSpan {
                    span: owner,
                    owner: continuation.owner,
                });
            }
            Some(SpanKind::Small(_)) | Some(SpanKind::LargeOwner(_)) | None => {
                return Err(SpanTableError::VacantSpan(owner));
            }
        };
        self.reserve_free_range_if_needed(owner.index())?;
        for ordinal in 0..span_count {
            let entry = &mut self.entries[owner.index() as usize + ordinal];
            entry.kind = None;
            entry.generation = entry.generation.next();
            self.insert_free_span(owner.index() + ordinal as u16);
        }
        self.live_spans -= span_count;
        Ok(LargeReclaim {
            span_count: span_count as u32,
            storage_bytes: span_count * crate::SPAN_SIZE_BYTES,
            object_bytes,
        })
    }

    /// Releases a complete already-dropped large owner/continuation range for exact reuse.
    pub fn reclaim_large_after_drop(
        &mut self,
        reference: RawHeapRef,
    ) -> Result<LargeReclaim, SpanTableError> {
        let owner = reference.span_id();
        if reference.span_offset().get() != crate::MINIMUM_SLOT_SIZE_BYTES as u16 {
            return Err(SpanTableError::LargeOwnerSpan(owner));
        }
        let (span_count, object_bytes) = match self
            .entries
            .get(owner.index() as usize)
            .and_then(|entry| entry.kind.as_ref())
        {
            Some(SpanKind::LargeOwner(span)) if span.metadata.is_allocated() => (
                span.metadata.span_count() as usize,
                span.metadata.object_bytes(),
            ),
            Some(SpanKind::LargeContinuation(continuation)) => {
                return Err(SpanTableError::LargeContinuationSpan {
                    span: owner,
                    owner: continuation.owner,
                });
            }
            Some(SpanKind::Small(_)) => return Err(SpanTableError::LargeOwnerSpan(owner)),
            Some(SpanKind::LargeOwner(_)) | None => return Err(SpanTableError::VacantSpan(owner)),
        };
        self.reserve_free_range_if_needed(owner.index())?;
        self.unpublish_large_for_drop(reference)?;
        let reclaimed = self.release_unpublished_large(reference)?;
        debug_assert_eq!(reclaimed.span_count(), span_count as u32);
        debug_assert_eq!(reclaimed.object_bytes(), object_bytes);
        Ok(reclaimed)
    }

    fn install_new(&mut self, mut span: SmallSpan) -> Result<SpanId, SpanTableError> {
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
        span.metadata.set_reuse_generation(generation);
        self.entries.push(SpanEntry {
            generation,
            kind: Some(SpanKind::Small(span)),
            remembered_next: None,
            in_remembered_set: false,
            young_next: None,
            in_young_set: false,
            in_eden_pool: false,
        });
        self.live_spans += 1;
        Ok(SpanId::new(index))
    }

    fn install_reused(&mut self, span_id: SpanId, mut span: SmallSpan) {
        let entry = &mut self.entries[span_id.index() as usize];
        debug_assert!(entry.kind.is_none());
        span.metadata.set_reuse_generation(entry.generation);
        entry.kind = Some(SpanKind::Small(span));
        entry.in_eden_pool = false;
        self.live_spans += 1;
    }

    /// Reserves either a free contiguous range or append capacity without publishing entries.
    fn reserve_span_range(
        &mut self,
        span_count: usize,
    ) -> Result<SpanRangePlacement, LargeAllocationError> {
        if let Some(owner) = self.take_free_span_range(span_count) {
            return Ok(SpanRangePlacement::Reused { owner });
        }
        let end = self
            .entries
            .len()
            .checked_add(span_count)
            .ok_or(LargeAllocationError::AddressSpaceExhausted)?;
        if end > MAX_LOGICAL_SPANS {
            return Err(LargeAllocationError::AddressSpaceExhausted);
        }
        reserve_for_additional(
            &mut self.entries,
            span_count,
            INITIAL_SPAN_TABLE_CAPACITY,
            MAX_LOGICAL_SPANS,
            SpanTableError::TableAllocationFailed,
        )
        .map_err(|_| LargeAllocationError::TableAllocationFailed)?;
        let owner = SpanId::new(
            u16::try_from(self.entries.len())
                .expect("range end was bounded by logical address space"),
        );
        Ok(SpanRangePlacement::Append { owner })
    }

    /// Installs owner and continuation metadata after storage and logical range reservation succeed.
    fn install_large(
        &mut self,
        placement: SpanRangePlacement,
        storage: SpanStorage,
        object_bytes: usize,
        span_count: usize,
    ) {
        let owner = placement.owner();
        match placement {
            SpanRangePlacement::Reused { .. } => {
                let generation = self.entries[owner.index() as usize].generation;
                self.entries[owner.index() as usize].kind = Some(SpanKind::LargeOwner(LargeSpan {
                    storage,
                    metadata: LargeSpanMetadata::new(span_count as u32, object_bytes, generation),
                }));
                for ordinal in 1..span_count {
                    self.entries[owner.index() as usize + ordinal].kind =
                        Some(SpanKind::LargeContinuation(LargeContinuation {
                            owner,
                            ordinal: ordinal as u32,
                        }));
                }
            }
            SpanRangePlacement::Append { .. } => {
                let generation = SpanReuseGeneration::INITIAL;
                self.entries.push(SpanEntry {
                    generation,
                    kind: Some(SpanKind::LargeOwner(LargeSpan {
                        storage,
                        metadata: LargeSpanMetadata::new(
                            span_count as u32,
                            object_bytes,
                            generation,
                        ),
                    })),
                    remembered_next: None,
                    in_remembered_set: false,
                    young_next: None,
                    in_young_set: false,
                    in_eden_pool: false,
                });
                for ordinal in 1..span_count {
                    self.entries.push(SpanEntry {
                        generation: SpanReuseGeneration::INITIAL,
                        kind: Some(SpanKind::LargeContinuation(LargeContinuation {
                            owner,
                            ordinal: ordinal as u32,
                        })),
                        remembered_next: None,
                        in_remembered_set: false,
                        young_next: None,
                        in_young_set: false,
                        in_eden_pool: false,
                    });
                }
            }
        }
        self.link_remembered_source(owner);
        self.live_spans += span_count;
    }

    fn take_free_span(&mut self) -> Option<SpanId> {
        self.take_free_span_range(1)
    }

    fn take_free_span_range(&mut self, span_count: usize) -> Option<SpanId> {
        let position = self.free_ranges.iter().rposition(|range| {
            usize::from(range.end) - usize::from(range.start) + 1 >= span_count
        })?;
        let range = &mut self.free_ranges[position];
        let owner = usize::from(range.end) + 1 - span_count;
        if owner == usize::from(range.start) {
            self.free_ranges.remove(position);
        } else {
            range.end = u16::try_from(owner - 1).expect("owner remains inside a u16 range");
        }
        Some(SpanId::new(
            u16::try_from(owner).expect("free range contains only logical span IDs"),
        ))
    }

    pub(super) fn reserve_free_range_if_needed(
        &mut self,
        index: u16,
    ) -> Result<(), SpanTableError> {
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

/// Reserves a centralized 1.5x capacity step before a push can mutate container state.
fn reserve_for_push<T>(
    values: &mut Vec<T>,
    initial_capacity: usize,
    maximum_capacity: usize,
    error: SpanTableError,
) -> Result<(), SpanTableError> {
    reserve_for_additional(values, 1, initial_capacity, maximum_capacity, error)
}

/// Reserves a bounded centralized growth step large enough for one transactional range append.
fn reserve_for_additional<T>(
    values: &mut Vec<T>,
    additional_values: usize,
    initial_capacity: usize,
    maximum_capacity: usize,
    error: SpanTableError,
) -> Result<(), SpanTableError> {
    let required = values.len().checked_add(additional_values).ok_or(error)?;
    if required <= values.capacity() {
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
    let target = target.max(required).min(maximum_capacity);
    if target < required {
        return Err(error);
    }
    let additional = target - values.len();
    values.try_reserve_exact(additional).map_err(|_| error)
}
