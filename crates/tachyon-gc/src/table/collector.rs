//! Collector-facing span views and non-moving cohort state transitions.

use super::{HeapReferenceError, SpanKind, SpanTable, SpanTableError};
use crate::{
    CollectionEpoch, RawHeapRef, SizeClass, SlotIndex, SmallSpanMetadata, SpanId, SpanSpace,
};

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

impl SpanTable {
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

    /// Links a stable entry at most once; pointer stores never allocate remembered-set storage.
    #[inline(always)]
    pub(super) fn link_remembered_source(&mut self, span_id: SpanId) {
        let entry = &mut self.entries[span_id.index() as usize];
        if entry.in_remembered_set {
            return;
        }
        entry.remembered_next = self.remembered_head;
        entry.in_remembered_set = true;
        self.remembered_head = Some(span_id);
    }

    /// Links a stable young owner at most once without an auxiliary allocation.
    pub(super) fn link_young_span(&mut self, span_id: SpanId) {
        let entry = &mut self.entries[span_id.index() as usize];
        if entry.in_young_set {
            return;
        }
        entry.young_next = self.young_head;
        entry.in_young_set = true;
        self.young_head = Some(span_id);
    }
}
