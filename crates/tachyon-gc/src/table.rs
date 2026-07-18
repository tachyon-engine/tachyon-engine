//! Stable logical span indexing over independently allocated native storage.

use core::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use crate::{
    CollectionEpoch, GcHeader, GcRef, GcTypeId, LargeSpanMetadata, MAX_LOGICAL_SPANS, ObjectLayout,
    RawHeapRef, SizeClass, SlotIndex, SmallObjectLayout, SmallObjectLayoutError, SmallSpanMetadata,
    SpanId, SpanReuseGeneration, SpanSpace, SpanStorage, SpanStorageAllocationError, Trace,
    TypeDescriptor,
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
    MetadataAllocationFailed,
    UnknownSpan(SpanId),
    VacantSpan(SpanId),
    LiveSpan(SpanId),
    LargeOwnerSpan(SpanId),
    LargeContinuationSpan { span: SpanId, owner: SpanId },
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

/// Copy-only collector view that never lends span storage across a callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SweepTarget {
    Small,
    LargeOwner,
}

/// Generation classification used by the stop-the-world young marker and write barrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReferenceSpace {
    Young,
    OldSmall,
    OldLarge,
}

/// Whole-span transition selected after a successful young sweep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum YoungSpanTransition {
    EdenToSurvivor,
    SurvivorAged,
    Promoted,
}

/// Stable small-span accounting captured between collector mutation steps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SmallSweepSnapshot {
    pub size_class: SizeClass,
    pub space: SpanSpace,
    pub bump_cursor: u16,
    pub allocated_slots: u16,
    pub allocated_bytes: u32,
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
    LargeOwnerOffset(RawHeapRef),
    LargeContinuationReference {
        reference: RawHeapRef,
        owner: SpanId,
        ordinal: u32,
    },
    PayloadAccess(RawHeapRef),
}

impl From<SpanStorageAllocationError> for SpanTableError {
    fn from(_: SpanStorageAllocationError) -> Self {
        Self::StorageAllocationFailed
    }
}

struct SmallSpan {
    storage: SpanStorage,
    metadata: MetadataBox<SmallSpanMetadata>,
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

struct MetadataBox<T>(Vec<T>);

impl<T> MetadataBox<T> {
    fn try_new(value: T) -> Result<Self, SpanTableError> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(1)
            .map_err(|_| SpanTableError::MetadataAllocationFailed)?;
        values.push(value);
        Ok(Self(values))
    }
}

impl<T> Deref for MetadataBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0[0]
    }
}

impl<T> DerefMut for MetadataBox<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0[0]
    }
}

struct LargeSpan {
    storage: SpanStorage,
    metadata: LargeSpanMetadata,
}

#[derive(Clone, Copy)]
struct LargeContinuation {
    owner: SpanId,
    ordinal: u32,
}

enum SpanKind {
    Small(SmallSpan),
    LargeOwner(LargeSpan),
    LargeContinuation(LargeContinuation),
}

struct SpanEntry {
    generation: SpanReuseGeneration,
    kind: Option<SpanKind>,
    remembered_next: Option<SpanId>,
    in_remembered_set: bool,
    young_next: Option<SpanId>,
    in_young_set: bool,
    in_eden_pool: bool,
}

/// A coalesced inclusive range of vacant logical span IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FreeSpanRange {
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

/// An isolate-local mapping whose entry indices remain stable across metadata-vector growth.
#[derive(Default)]
pub struct SpanTable {
    entries: Vec<SpanEntry>,
    free_ranges: Vec<FreeSpanRange>,
    live_spans: usize,
    remembered_head: Option<SpanId>,
    young_head: Option<SpanId>,
    active_young_spans: usize,
}

impl SpanTable {
    /// Creates an empty table; the first slow-path insertion performs the educated reservation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            free_ranges: Vec::new(),
            live_spans: 0,
            remembered_head: None,
            young_head: None,
            active_young_spans: 0,
        }
    }

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

    /// Resolves through the current table entry every time instead of caching a native object pointer.
    #[must_use]
    pub fn base_address(&self, span_id: SpanId) -> Option<*const u8> {
        match self.entries.get(span_id.index() as usize)?.kind.as_ref()? {
            SpanKind::Small(span) => Some(span.storage.base_address()),
            SpanKind::LargeOwner(span) => Some(span.storage.base_address()),
            SpanKind::LargeContinuation(continuation) => {
                let owner = self.entries.get(continuation.owner.index() as usize)?;
                let SpanKind::LargeOwner(span) = owner.kind.as_ref()? else {
                    return None;
                };
                Some(
                    span.storage
                        .base_address()
                        .wrapping_add(continuation.ordinal as usize * crate::SPAN_SIZE_BYTES),
                )
            }
        }
    }

    /// Returns immutable side metadata for collector and verifier operations.
    #[must_use]
    pub fn metadata(&self, span_id: SpanId) -> Option<&SmallSpanMetadata> {
        let SpanKind::Small(span) = self.entries.get(span_id.index() as usize)?.kind.as_ref()?
        else {
            return None;
        };
        Some(&span.metadata)
    }

    /// Returns mutable side metadata while preserving exclusive isolate ownership.
    #[must_use]
    pub fn metadata_mut(&mut self, span_id: SpanId) -> Option<&mut SmallSpanMetadata> {
        let SpanKind::Small(span) = self
            .entries
            .get_mut(span_id.index() as usize)?
            .kind
            .as_mut()?
        else {
            return None;
        };
        Some(&mut span.metadata)
    }

    /// Returns owner metadata only when the ID names the beginning of a large range.
    #[must_use]
    pub fn large_metadata(&self, span_id: SpanId) -> Option<LargeSpanMetadata> {
        let SpanKind::LargeOwner(span) =
            self.entries.get(span_id.index() as usize)?.kind.as_ref()?
        else {
            return None;
        };
        Some(span.metadata)
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

    /// Counts only non-empty object backing spans, excluding retained empty Eden pool storage.
    pub(crate) fn live_object_span_counts(&self) -> (usize, usize) {
        let mut young = 0_usize;
        let mut old = 0_usize;
        for entry in &self.entries {
            match entry.kind.as_ref() {
                Some(SpanKind::Small(span)) if span.metadata.allocated_slots() != 0 => {
                    match span.metadata.space() {
                        SpanSpace::Eden | SpanSpace::Survivor { .. } => young += 1,
                        SpanSpace::Old => old += 1,
                    }
                }
                Some(SpanKind::LargeOwner(span)) if span.metadata.is_allocated() => {
                    old += span.metadata.span_count() as usize;
                }
                Some(SpanKind::Small(_) | SpanKind::LargeOwner(_))
                | Some(SpanKind::LargeContinuation(_))
                | None => {}
            }
        }
        (young, old)
    }

    /// Classifies an occupied owner entry while excluding vacant and continuation IDs.
    #[must_use]
    pub(crate) fn sweep_target(&self, span_id: SpanId) -> Option<SweepTarget> {
        match self.entries.get(span_id.index() as usize)?.kind.as_ref()? {
            SpanKind::Small(_) => Some(SweepTarget::Small),
            SpanKind::LargeOwner(_) => Some(SweepTarget::LargeOwner),
            SpanKind::LargeContinuation(_) => None,
        }
    }

    /// Captures only scalar metadata needed to account for one small-span sweep.
    #[must_use]
    pub(crate) fn small_sweep_snapshot(&self, span_id: SpanId) -> Option<SmallSweepSnapshot> {
        let metadata = self.metadata(span_id)?;
        Some(SmallSweepSnapshot {
            size_class: metadata.size_class(),
            space: metadata.space(),
            bump_cursor: metadata.bump_cursor(),
            allocated_slots: metadata.allocated_slots(),
            allocated_bytes: metadata.allocated_bytes(),
        })
    }

    /// Returns the next live logical reference from the allocation bitmap without allocating.
    #[must_use]
    pub(crate) fn next_allocated_reference(
        &self,
        span_id: SpanId,
        start: u16,
    ) -> Option<RawHeapRef> {
        let metadata = self.metadata(span_id)?;
        let slot = metadata.allocations().next_allocated(start)?;
        let offset = metadata
            .size_class()
            .offset_for_slot(slot)
            .expect("allocated bitmap bits belong to the span size class");
        Some(RawHeapRef::from_parts(span_id, offset))
    }

    /// Classifies a verified object without exposing its native storage address.
    pub(crate) fn reference_space(
        &self,
        reference: RawHeapRef,
    ) -> Result<ReferenceSpace, HeapReferenceError> {
        self.verify_reference(reference, None)?;
        let kind = self.entries[reference.span_id().index() as usize]
            .kind
            .as_ref()
            .expect("verified references have occupied entries");
        Ok(match kind {
            SpanKind::Small(span) => match span.metadata.space() {
                SpanSpace::Eden | SpanSpace::Survivor { .. } => ReferenceSpace::Young,
                SpanSpace::Old => ReferenceSpace::OldSmall,
            },
            SpanKind::LargeOwner(_) => ReferenceSpace::OldLarge,
            SpanKind::LargeContinuation(_) => unreachable!("verifier rejects continuations"),
        })
    }

    /// Records the only Phase 1B remembered-set transition: an Old source storing a young target.
    #[inline(always)]
    pub(crate) fn remember_old_to_young(
        &mut self,
        source: RawHeapRef,
        target: RawHeapRef,
    ) -> Result<bool, HeapReferenceError> {
        let source_space = self.reference_space(source)?;
        if self.reference_space(target)? != ReferenceSpace::Young {
            return Ok(false);
        }
        let kind = self.entries[source.span_id().index() as usize]
            .kind
            .as_mut()
            .expect("verified source has an occupied entry");
        let changed = match (source_space, kind) {
            (ReferenceSpace::Young, _) => false,
            (ReferenceSpace::OldSmall, SpanKind::Small(span)) => {
                span.metadata.remember_card(source.span_offset())
            }
            (ReferenceSpace::OldLarge, SpanKind::LargeOwner(span)) => {
                let changed = !span.metadata.is_remembered();
                span.metadata.set_remembered(true);
                changed
            }
            _ => unreachable!("source classification matches its occupied entry"),
        };
        if changed {
            self.link_remembered_source(source.span_id());
        }
        Ok(changed)
    }

    /// Returns the first owner in the allocation-free remembered-source chain.
    #[must_use]
    pub(crate) const fn remembered_head(&self) -> Option<SpanId> {
        self.remembered_head
    }

    /// Advances through only sources that have entered the remembered set.
    #[must_use]
    pub(crate) fn remembered_next(&self, span_id: SpanId) -> Option<SpanId> {
        self.entries
            .get(span_id.index() as usize)
            .and_then(|entry| entry.remembered_next)
    }

    /// Reports intrusive-chain membership independently from card/large remembered bits.
    #[must_use]
    pub(crate) fn is_in_remembered_set(&self, span_id: SpanId) -> bool {
        self.entries
            .get(span_id.index() as usize)
            .is_some_and(|entry| entry.in_remembered_set)
    }

    /// Returns the first Eden/Survivor owner in the allocation-free young-span chain.
    #[must_use]
    pub(crate) const fn young_head(&self) -> Option<SpanId> {
        self.young_head
    }

    /// Advances through only spans allocated into a young cohort.
    #[must_use]
    pub(crate) fn young_next(&self, span_id: SpanId) -> Option<SpanId> {
        self.entries
            .get(span_id.index() as usize)
            .and_then(|entry| entry.young_next)
    }

    /// Returns a young cohort only for a currently live small span.
    #[must_use]
    pub(crate) fn young_space(&self, span_id: SpanId) -> Option<SpanSpace> {
        let SpanKind::Small(span) = self.entries.get(span_id.index() as usize)?.kind.as_ref()?
        else {
            return None;
        };
        match span.metadata.space() {
            space @ (SpanSpace::Eden | SpanSpace::Survivor { .. }) => Some(space),
            SpanSpace::Old => None,
        }
    }

    /// Returns current-epoch live occupancy for a young span without scanning object payloads.
    pub(crate) fn young_live_occupancy(
        &self,
        span_id: SpanId,
        epoch: CollectionEpoch,
    ) -> Option<(u16, u16)> {
        let metadata = self.metadata(span_id)?;
        matches!(
            metadata.space(),
            SpanSpace::Eden | SpanSpace::Survivor { .. }
        )
        .then(|| {
            (
                metadata.marks().marked_count(epoch),
                metadata.size_class().slot_count(),
            )
        })
    }

    /// Returns active Eden/Survivor backing in O(1), excluding detached empty pool spans.
    pub(crate) fn young_storage_bytes(&self) -> usize {
        self.active_young_spans
            .saturating_mul(crate::SPAN_SIZE_BYTES)
    }

    /// Ages a non-empty young span and activates prepared cards on whole-span promotion.
    pub(crate) fn advance_young_cohort(
        &mut self,
        span_id: SpanId,
        promotion_age: u8,
        promote_early: bool,
    ) -> YoungSpanTransition {
        let transition = {
            let Some(SpanKind::Small(span)) = self
                .entries
                .get_mut(span_id.index() as usize)
                .and_then(|entry| entry.kind.as_mut())
            else {
                unreachable!("young chain contains a live small span");
            };
            let space = span.metadata.space();
            if promote_early || crate::tuning::promotion_due_to_age(space, promotion_age) {
                span.metadata.set_space(SpanSpace::Old);
                YoungSpanTransition::Promoted
            } else {
                match space {
                    SpanSpace::Eden => {
                        span.metadata.set_space(SpanSpace::Survivor { age: 1 });
                        YoungSpanTransition::EdenToSurvivor
                    }
                    SpanSpace::Survivor { age } => {
                        let next_age = age.saturating_add(1);
                        span.metadata
                            .set_space(SpanSpace::Survivor { age: next_age });
                        YoungSpanTransition::SurvivorAged
                    }
                    SpanSpace::Old => unreachable!("young chain excludes Old spans"),
                }
            }
        };
        if transition == YoungSpanTransition::Promoted && self.dirty_old_card_count(span_id) != 0 {
            self.link_remembered_source(span_id);
        }
        self.active_young_spans = self
            .active_young_spans
            .checked_sub(usize::from(transition == YoungSpanTransition::Promoted))
            .expect("promotion cannot underflow active young span accounting");
        transition
    }

    /// Installs exact cards prepared while a promotion candidate still had young mark state.
    pub(crate) fn replace_promotion_cards(&mut self, span_id: SpanId, cards: crate::CardBitmap) {
        let Some(SpanKind::Small(span)) = self
            .entries
            .get_mut(span_id.index() as usize)
            .and_then(|entry| entry.kind.as_mut())
        else {
            return;
        };
        debug_assert!(matches!(span.metadata.space(), SpanSpace::Survivor { .. }));
        span.metadata.replace_cards(cards);
    }

    /// Removes released and promoted entries from the intrusive young chain in one bounded pass.
    pub(crate) fn compact_young_spans(&mut self) {
        let mut current = self.young_head.take();
        while let Some(span_id) = current {
            let index = span_id.index() as usize;
            let next = self.entries[index].young_next.take();
            self.entries[index].in_young_set = false;
            if self.young_space(span_id).is_some() && !self.entries[index].in_eden_pool {
                self.link_young_span(span_id);
            }
            current = next;
        }
    }

    /// Resets an empty young span for pool retention without releasing its allocator backing.
    pub(crate) fn pool_empty_young(&mut self, span_id: SpanId) {
        let entry = &mut self.entries[span_id.index() as usize];
        let Some(SpanKind::Small(span)) = entry.kind.as_mut() else {
            unreachable!("young sweep only pools a live small span");
        };
        assert_eq!(span.metadata.allocated_slots(), 0);
        assert_ne!(span.metadata.space(), SpanSpace::Old);
        entry.generation = entry.generation.next();
        *span.metadata = SmallSpanMetadata::new(
            span.metadata.size_class(),
            SpanSpace::Eden,
            entry.generation,
        );
        entry.in_eden_pool = true;
        self.active_young_spans = self
            .active_young_spans
            .checked_sub(1)
            .expect("pooling requires one active young span");
    }

    /// Reattaches a retained empty Eden span to allocation and young-collection ownership.
    pub(crate) fn activate_pooled_eden(&mut self, span_id: SpanId) {
        let entry = &mut self.entries[span_id.index() as usize];
        let Some(SpanKind::Small(span)) = entry.kind.as_ref() else {
            unreachable!("pool IDs name retained small spans");
        };
        assert!(entry.in_eden_pool);
        assert_eq!(span.metadata.space(), SpanSpace::Eden);
        assert_eq!(span.metadata.allocated_slots(), 0);
        entry.in_eden_pool = false;
        self.active_young_spans = self
            .active_young_spans
            .checked_add(1)
            .expect("pool activation cannot exceed the logical span table");
        self.link_young_span(span_id);
    }

    /// Returns dirty-card count only for an Old small span.
    #[must_use]
    pub(crate) fn dirty_old_card_count(&self, span_id: SpanId) -> usize {
        let Some(SpanKind::Small(span)) = self
            .entries
            .get(span_id.index() as usize)
            .and_then(|entry| entry.kind.as_ref())
        else {
            return 0;
        };
        if span.metadata.space() == SpanSpace::Old {
            span.metadata.cards().dirty_count()
        } else {
            0
        }
    }

    /// Returns the next allocated Old object whose source card is dirty.
    #[must_use]
    pub(crate) fn next_dirty_old_reference(
        &self,
        span_id: SpanId,
        start: u16,
    ) -> Option<RawHeapRef> {
        let span = match self.entries.get(span_id.index() as usize)?.kind.as_ref()? {
            SpanKind::Small(span) if span.metadata.space() == SpanSpace::Old => span,
            _ => return None,
        };
        let mut next = start;
        while let Some(slot) = span.metadata.allocations().next_allocated(next) {
            let offset = span
                .metadata
                .size_class()
                .offset_for_slot(slot)
                .expect("allocated slots belong to their size class");
            if span.metadata.cards().is_dirty(offset) {
                return Some(RawHeapRef::from_parts(span_id, offset));
            }
            next = slot.index().checked_add(1)?;
        }
        None
    }

    /// Replaces one Old span's card snapshot after all source traces succeeded.
    pub(crate) fn replace_old_cards(&mut self, span_id: SpanId, cards: crate::CardBitmap) {
        let Some(SpanKind::Small(span)) = self
            .entries
            .get_mut(span_id.index() as usize)
            .and_then(|entry| entry.kind.as_mut())
        else {
            return;
        };
        debug_assert_eq!(span.metadata.space(), SpanSpace::Old);
        span.metadata.replace_cards(cards);
    }

    /// Returns the canonical reference when a large owner remains in the remembered set.
    #[must_use]
    pub(crate) fn remembered_large_reference(&self, span_id: SpanId) -> Option<RawHeapRef> {
        let SpanKind::LargeOwner(span) =
            self.entries.get(span_id.index() as usize)?.kind.as_ref()?
        else {
            return None;
        };
        span.metadata.is_remembered().then(|| {
            RawHeapRef::from_parts(
                span_id,
                crate::SpanOffset::new(crate::MINIMUM_SLOT_SIZE_BYTES as u16)
                    .expect("large owner offset is non-zero"),
            )
        })
    }

    /// Replaces conservative owner-level remembered state after a direct-edge rescan.
    pub(crate) fn set_large_remembered(&mut self, span_id: SpanId, remembered: bool) {
        let Some(SpanKind::LargeOwner(span)) = self
            .entries
            .get_mut(span_id.index() as usize)
            .and_then(|entry| entry.kind.as_mut())
        else {
            return;
        };
        span.metadata.set_remembered(remembered);
    }

    /// Removes clean and released owners from the intrusive chain without allocating a side vector.
    pub(crate) fn compact_remembered_sources(&mut self) {
        let mut current = self.remembered_head.take();
        while let Some(span_id) = current {
            let index = span_id.index() as usize;
            let next = self.entries[index].remembered_next.take();
            self.entries[index].in_remembered_set = false;
            let retain = match self.entries[index].kind.as_ref() {
                Some(SpanKind::Small(span)) => {
                    span.metadata.space() == SpanSpace::Old
                        && span.metadata.cards().dirty_count() != 0
                }
                Some(SpanKind::LargeOwner(span)) => span.metadata.is_remembered(),
                Some(SpanKind::LargeContinuation(_)) | None => false,
            };
            if retain {
                self.link_remembered_source(span_id);
            }
            current = next;
        }
    }

    /// Ensures a later whole-span/range release cannot fail after payload destruction.
    pub(crate) fn prepare_release(&mut self, span_id: SpanId) -> Result<(), SpanTableError> {
        self.reserve_free_range_if_needed(span_id.index())
    }

    /// Rebuilds an Old span's complete free list from allocation bits after batch sweep.
    pub(crate) fn rebuild_old_free_list(&mut self, span_id: SpanId) {
        let Some(SpanKind::Small(span)) = self
            .entries
            .get_mut(span_id.index() as usize)
            .and_then(|entry| entry.kind.as_mut())
        else {
            return;
        };
        if span.metadata.space() != SpanSpace::Old {
            return;
        }
        span.metadata.set_free_list_head(None);
        for index in 0..span.metadata.bump_cursor() {
            let slot = SlotIndex::new(index).expect("bump cursor is bounded by the size class");
            if span.metadata.allocations().is_allocated(slot) {
                continue;
            }
            let offset = span
                .metadata
                .size_class()
                .offset_for_slot(slot)
                .expect("bump slots belong to the size class");
            let previous = span.metadata.free_list_head();
            span.storage.write_free_next(offset, previous);
            span.metadata.set_free_list_head(Some(slot));
        }
    }

    /// Checks the active-span fast path without consuming the value that a slow path may need.
    #[must_use]
    #[inline(always)]
    pub fn can_allocate_in_span(&self, span_id: SpanId) -> bool {
        self.entries
            .get(span_id.index() as usize)
            .and_then(|entry| entry.kind.as_ref())
            .is_some_and(|kind| match kind {
                SpanKind::Small(span) => span.metadata.has_allocation_capacity(),
                SpanKind::LargeOwner(_) | SpanKind::LargeContinuation(_) => false,
            })
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
        let kind = self
            .entries
            .get_mut(span_id.index() as usize)
            .ok_or(SmallAllocationError::UnknownSpan(span_id))?
            .kind
            .as_mut()
            .ok_or(SmallAllocationError::VacantSpan(span_id))?;
        let SpanKind::Small(span) = kind else {
            return Err(SmallAllocationError::SpanFull(span_id));
        };
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
        let remember =
            span.metadata.space() == SpanSpace::Old && span.metadata.remember_card(offset);
        let reference = GcRef::from_raw(RawHeapRef::from_parts(span_id, offset));
        if remember {
            self.link_remembered_source(span_id);
        }
        Ok(reference)
    }

    /// Verifies a live small or large owner without native page faults or cached pointers.
    pub fn verify_reference(
        &self,
        reference: RawHeapRef,
        expected_type: Option<GcTypeId>,
    ) -> Result<GcHeader, HeapReferenceError> {
        let span_id = reference.span_id();
        let entry = self
            .entries
            .get(span_id.index() as usize)
            .ok_or(HeapReferenceError::UnknownSpan(span_id))?;
        let kind = entry
            .kind
            .as_ref()
            .ok_or(HeapReferenceError::VacantSpan(span_id))?;
        let header = match kind {
            SpanKind::Small(span) => verify_small_span(span, reference)?,
            SpanKind::LargeOwner(span) => {
                if reference.span_offset().get() != crate::MINIMUM_SLOT_SIZE_BYTES as u16 {
                    return Err(HeapReferenceError::LargeOwnerOffset(reference));
                }
                if !span.metadata.is_allocated() {
                    return Err(HeapReferenceError::UnallocatedSlot(reference));
                }
                span.storage
                    .header(reference.span_offset())
                    .expect("large owner storage contains a complete header")
            }
            SpanKind::LargeContinuation(continuation) => {
                return Err(HeapReferenceError::LargeContinuationReference {
                    reference,
                    owner: continuation.owner,
                    ordinal: continuation.ordinal,
                });
            }
        };
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

    /// Returns mark state after the same exact live-reference checks used by the debug verifier.
    pub(crate) fn is_marked(
        &self,
        reference: RawHeapRef,
        epoch: CollectionEpoch,
    ) -> Result<bool, HeapReferenceError> {
        self.verify_reference(reference, None)?;
        let kind = self.entries[reference.span_id().index() as usize]
            .kind
            .as_ref()
            .expect("verified references have occupied entries");
        Ok(match kind {
            SpanKind::Small(span) => {
                let slot = span
                    .metadata
                    .size_class()
                    .slot_for_offset(reference.span_offset())
                    .expect("verified small reference is on a slot boundary");
                span.metadata.marks().is_marked(slot, epoch)
            }
            SpanKind::LargeOwner(span) => span.metadata.is_marked(epoch),
            SpanKind::LargeContinuation(_) => unreachable!("verifier rejects continuation roots"),
        })
    }

    /// Changes white to gray after the caller has reserved queue capacity.
    pub(crate) fn mark_reference(
        &mut self,
        reference: RawHeapRef,
        epoch: CollectionEpoch,
    ) -> Result<bool, HeapReferenceError> {
        self.verify_reference(reference, None)?;
        let kind = self.entries[reference.span_id().index() as usize]
            .kind
            .as_mut()
            .expect("verified references have occupied entries");
        Ok(match kind {
            SpanKind::Small(span) => {
                let slot = span
                    .metadata
                    .size_class()
                    .slot_for_offset(reference.span_offset())
                    .expect("verified small reference is on a slot boundary");
                span.metadata.marks_mut().mark(slot, epoch)
            }
            SpanKind::LargeOwner(span) => span.metadata.mark(epoch),
            SpanKind::LargeContinuation(_) => unreachable!("verifier rejects continuation roots"),
        })
    }

    /// Resolves the descriptor-checked payload immediately before a trace callback invocation.
    pub(crate) fn payload_address(
        &mut self,
        reference: RawHeapRef,
        descriptor: TypeDescriptor,
    ) -> Result<NonNull<u8>, HeapReferenceError> {
        self.verify_reference(reference, Some(descriptor.type_id()))?;
        let kind = self.entries[reference.span_id().index() as usize]
            .kind
            .as_mut()
            .expect("verified references have occupied entries");
        let storage = match kind {
            SpanKind::Small(span) => &mut span.storage,
            SpanKind::LargeOwner(span) => &mut span.storage,
            SpanKind::LargeContinuation(_) => unreachable!("verifier rejects continuations"),
        };
        storage
            .payload_address(reference.span_offset(), descriptor.layout())
            .map_err(|_| HeapReferenceError::PayloadAccess(reference))
    }

    /// Resolves a descriptor-checked shared payload without lending table storage directly.
    pub(crate) fn payload_address_shared(
        &self,
        reference: RawHeapRef,
        descriptor: TypeDescriptor,
    ) -> Result<*const u8, HeapReferenceError> {
        self.verify_reference(reference, Some(descriptor.type_id()))?;
        let kind = self.entries[reference.span_id().index() as usize]
            .kind
            .as_ref()
            .expect("verified references have occupied entries");
        let storage = match kind {
            SpanKind::Small(span) => &span.storage,
            SpanKind::LargeOwner(span) => &span.storage,
            SpanKind::LargeContinuation(_) => unreachable!("verifier rejects continuations"),
        };
        storage
            .payload_address_shared(reference.span_offset(), descriptor.layout())
            .map_err(|_| HeapReferenceError::PayloadAccess(reference))
    }

    /// Reclaims an object after its descriptor drop callback has completed.
    ///
    /// The collector owns sequencing: calling this early leaks Rust resources when the slot is
    /// overwritten, but cannot expose a safe typed borrow because resolution remains scope-owned.
    pub fn reclaim_small_after_drop(&mut self, reference: RawHeapRef) -> bool {
        let Some(span) = self
            .entries
            .get_mut(reference.span_id().index() as usize)
            .and_then(|entry| entry.kind.as_mut())
            .and_then(|kind| match kind {
                SpanKind::Small(span) => Some(span),
                SpanKind::LargeOwner(_) | SpanKind::LargeContinuation(_) => None,
            })
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

    /// Removes lifetime metadata before invoking a Rust destructor.
    ///
    /// Storage bytes remain untouched until the callback returns. Production builds abort if a
    /// destructor panics, so collection never attempts recovery or a second destructor call.
    pub(crate) fn unpublish_small_for_drop(&mut self, reference: RawHeapRef) -> bool {
        let Some(span) = self
            .entries
            .get_mut(reference.span_id().index() as usize)
            .and_then(|entry| entry.kind.as_mut())
            .and_then(|kind| match kind {
                SpanKind::Small(span) => Some(span),
                SpanKind::LargeOwner(_) | SpanKind::LargeContinuation(_) => None,
            })
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
        span.metadata.reclaim_allocation(slot)
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

    /// Advances the epoch, physically resetting every live span bitmap on the forced-wrap path.
    pub fn advance_collection_epoch(&mut self, current: CollectionEpoch) -> CollectionEpoch {
        match current.next() {
            Ok(next) => next,
            Err(_) => {
                for entry in &mut self.entries {
                    match &mut entry.kind {
                        Some(SpanKind::Small(span)) => {
                            span.metadata.marks_mut().reset_for_epoch_overflow();
                        }
                        Some(SpanKind::LargeOwner(span)) => span.metadata.reset_mark_epoch(),
                        Some(SpanKind::LargeContinuation(_)) | None => {}
                    }
                }
                CollectionEpoch::INITIAL
            }
        }
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

    /// Links a stable entry at most once; pointer stores never allocate remembered-set storage.
    #[inline(always)]
    fn link_remembered_source(&mut self, span_id: SpanId) {
        let entry = &mut self.entries[span_id.index() as usize];
        if entry.in_remembered_set {
            return;
        }
        entry.remembered_next = self.remembered_head;
        entry.in_remembered_set = true;
        self.remembered_head = Some(span_id);
    }

    /// Links a stable young owner at most once without an auxiliary allocation.
    fn link_young_span(&mut self, span_id: SpanId) {
        let entry = &mut self.entries[span_id.index() as usize];
        if entry.in_young_set {
            return;
        }
        entry.young_next = self.young_head;
        entry.in_young_set = true;
        self.young_head = Some(span_id);
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

fn verify_small_span(
    span: &SmallSpan,
    reference: RawHeapRef,
) -> Result<GcHeader, HeapReferenceError> {
    let slot = span
        .metadata
        .size_class()
        .slot_for_offset(reference.span_offset())
        .ok_or(HeapReferenceError::InvalidSlotBoundary(reference))?;
    if !span.metadata.allocations().is_allocated(slot) {
        return Err(HeapReferenceError::UnallocatedSlot(reference));
    }
    Ok(span
        .storage
        .header(reference.span_offset())
        .expect("validated small-object slots always contain a complete header"))
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
            .verify_reference(reference.raw(), Some(type_id))
            .unwrap();
        assert_eq!(header.type_id(), Some(type_id));
        assert_eq!(header.flags(), 0x55aa);
        assert_eq!(header.aux(), 17);
        assert_eq!(table.metadata(span).unwrap().allocated_slots(), 1);
        assert_eq!(table.release(span), Err(SpanTableError::LiveSpan(span)));

        let wrong_type = GcTypeId::new(10).unwrap();
        assert_eq!(
            table.verify_reference(reference.raw(), Some(wrong_type)),
            Err(HeapReferenceError::TypeMismatch {
                expected: wrong_type,
                actual: type_id,
            })
        );
        let unallocated = RawHeapRef::from_parts(span, SpanOffset::new(32).unwrap());
        assert_eq!(
            table.verify_reference(unallocated, None),
            Err(HeapReferenceError::UnallocatedSlot(unallocated))
        );
        let misaligned = RawHeapRef::from_parts(span, SpanOffset::new(17).unwrap());
        assert_eq!(
            table.verify_reference(misaligned, None),
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
