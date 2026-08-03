//! Bounded span-at-a-time full-major sweeping after the strong mark fixed point.

use crate::{
    CollectionEpoch, HeapReferenceError, MINIMUM_SLOT_SIZE_BYTES, RawHeapRef, SPAN_SIZE_BYTES,
    SpanId, SpanSpace, SpanTable, SpanTableError, TypeRegistry,
    eden::EdenPool,
    table::{SmallSweepSnapshot, SweepTarget, YoungSpanTransition},
    tuning::{
        CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_SWEEP_WORKLIST_CAPACITY,
        SMALL_SIZE_CLASSES,
    },
};

/// A span owner could not be retained within the bounded sweep worklist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SweepWorklistError {
    EntryLimitExceeded { limit: usize },
    AllocationFailed,
}

/// Retained high-water evidence for tuning the span-level sweep queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SweepWorklistStats {
    pub initial_capacity: usize,
    pub growth_count: usize,
    pub peak_len: usize,
    pub retained_capacity: usize,
    pub slack_entries: usize,
}

/// A full-sweep failure leaves already reclaimed objects unpublished and never double-dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SweepError {
    Worklist(SweepWorklistError),
    InvalidReference(HeapReferenceError),
    UnknownTypeId(RawHeapRef),
    SpanTable(SpanTableError),
    AllocationStateChanged(RawHeapRef),
    ExternalAccountingUnderflow {
        reference: RawHeapRef,
        charged: usize,
        available: usize,
    },
}

/// Deterministic object, byte, span, and retained-fragmentation counters for one full sweep.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SweepStats {
    pub scanned_objects: usize,
    pub live_objects: usize,
    pub reclaimed_objects: usize,
    pub scanned_bytes: usize,
    pub live_bytes: usize,
    pub reclaimed_bytes: usize,
    pub spans_processed: usize,
    pub spans_released: usize,
    pub released_storage_bytes: usize,
    pub external_bytes: usize,
    pub retained_fragmentation_bytes: usize,
    pub reusable_old_free_bytes: usize,
    pub retained_tail_slack_bytes: usize,
    pub allocated_young_bytes_total: usize,
    pub allocated_old_bytes_total: usize,
    pub young_live_spans: usize,
    pub old_live_spans: usize,
    pub eden_pool_retained_bytes: usize,
}

/// Young-only sweep and cohort-transition counters for one minor collection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MinorSweepStats {
    pub sweep: SweepStats,
    pub eden_spans_processed: usize,
    pub survivor_spans_processed: usize,
    pub eden_to_survivor: usize,
    pub survivor_spans_aged: usize,
    pub whole_span_promotions: usize,
    pub early_whole_span_promotions: usize,
    pub eden_spans_pooled: usize,
    pub eden_pool_overflow_spans_released: usize,
    pub eden_pool_retained_bytes: usize,
}

/// LIFO owner IDs keep collection memory proportional to spans rather than objects.
pub(crate) struct SweepWorklist {
    entries: Vec<SpanId>,
    max_entries: usize,
    initial_capacity: usize,
    growth_count: usize,
    peak_len: usize,
}

impl SweepWorklist {
    pub const fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
            initial_capacity: 0,
            growth_count: 0,
            peak_len: 0,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Applies the centralized 1.5x policy before publishing another span owner.
    pub fn try_push(&mut self, span_id: SpanId) -> Result<(), SweepWorklistError> {
        if self.entries.len() == self.max_entries {
            return Err(SweepWorklistError::EntryLimitExceeded {
                limit: self.max_entries,
            });
        }
        if self.entries.len() == self.entries.capacity() {
            let target = if self.entries.capacity() == 0 {
                INITIAL_SWEEP_WORKLIST_CAPACITY.min(self.max_entries)
            } else {
                self.entries
                    .capacity()
                    .saturating_mul(CAPACITY_GROWTH_NUMERATOR)
                    .div_ceil(CAPACITY_GROWTH_DENOMINATOR)
                    .min(self.max_entries)
            }
            .max(self.entries.len() + 1);
            self.entries
                .try_reserve_exact(target - self.entries.len())
                .map_err(|_| SweepWorklistError::AllocationFailed)?;
            if self.initial_capacity == 0 {
                self.initial_capacity = self.entries.capacity();
            } else {
                self.growth_count += 1;
            }
        }
        self.entries.push(span_id);
        self.peak_len = self.peak_len.max(self.entries.len());
        Ok(())
    }

    pub fn pop(&mut self) -> Option<SpanId> {
        self.entries.pop()
    }

    pub fn stats(&self) -> SweepWorklistStats {
        SweepWorklistStats {
            initial_capacity: self.initial_capacity,
            growth_count: self.growth_count,
            peak_len: self.peak_len,
            retained_capacity: self.entries.capacity(),
            slack_entries: self.entries.capacity() - self.entries.len(),
        }
    }
}

/// Sweeps every owner present when the phase begins; no dead-object side vector is created.
pub(crate) fn sweep_full(
    table: &mut SpanTable,
    types: &TypeRegistry,
    worklist: &mut SweepWorklist,
    epoch: CollectionEpoch,
    external_bytes: &mut usize,
    stats: &mut SweepStats,
) -> Result<(), SweepError> {
    worklist.clear();
    for index in 0..table.historical_span_count() {
        let span_id = SpanId::new(index as u16);
        if table.sweep_target(span_id).is_some() {
            worklist.try_push(span_id).map_err(SweepError::Worklist)?;
        }
    }

    while let Some(span_id) = worklist.pop() {
        stats.spans_processed += 1;
        match table.sweep_target(span_id) {
            Some(SweepTarget::Small) => {
                sweep_small(table, types, span_id, epoch, external_bytes, stats)?;
            }
            Some(SweepTarget::LargeOwner) => {
                sweep_large(table, types, span_id, epoch, external_bytes, stats)?;
            }
            None => {}
        }
    }
    Ok(())
}

/// Drops every payload still published when its owning heap is destroyed.
///
/// Teardown deliberately ignores reachability and does not allocate: Rust-owned backing inside
/// live GC payloads must be released even though ordinary roots would retain those objects during
/// a major collection. Span storage remains installed until the surrounding `SpanTable` drops.
pub(crate) fn teardown_payloads(
    table: &mut SpanTable,
    types: &TypeRegistry,
    external_bytes: &mut usize,
) -> Result<(), SweepError> {
    for index in 0..table.historical_span_count() {
        let span_id = SpanId::new(index as u16);
        match table.sweep_target(span_id) {
            Some(SweepTarget::Small) => {
                while let Some(reference) = table.next_allocated_reference(span_id, 0) {
                    drop_small(table, types, reference, external_bytes)?;
                }
            }
            Some(SweepTarget::LargeOwner) => {
                drop_large_payload_for_teardown(table, types, span_id, external_bytes)?;
            }
            None => {}
        }
    }
    Ok(())
}

/// Drops one still-published large payload without rebuilding table free ranges during teardown.
fn drop_large_payload_for_teardown(
    table: &mut SpanTable,
    types: &TypeRegistry,
    span_id: SpanId,
    external_bytes: &mut usize,
) -> Result<(), SweepError> {
    let Some(metadata) = table.large_metadata(span_id) else {
        return Ok(());
    };
    if !metadata.is_allocated() {
        return Ok(());
    }
    let reference = RawHeapRef::from_parts(
        span_id,
        crate::SpanOffset::new(MINIMUM_SLOT_SIZE_BYTES as u16)
            .expect("large owner offset is non-zero"),
    );
    let header = table
        .verify_reference(reference, None)
        .map_err(SweepError::InvalidReference)?;
    let type_id = header
        .type_id()
        .ok_or(SweepError::UnknownTypeId(reference))?;
    let descriptor = types
        .descriptor(type_id)
        .ok_or(SweepError::UnknownTypeId(reference))?;
    let payload = table
        .payload_address(reference, descriptor)
        .map_err(SweepError::InvalidReference)?;
    let charged = header.external_bytes().unwrap_or(0);
    ensure_external_charge_available(reference, charged, *external_bytes)?;
    table
        .unpublish_large_for_drop(reference)
        .map_err(SweepError::SpanTable)?;
    *external_bytes -= charged;
    // SAFETY: the verified owner header selects this immutable descriptor and payload layout;
    // teardown retains the complete backing range until the `SpanTable` field is dropped.
    unsafe { descriptor.drop(payload) };
    Ok(())
}

struct YoungSweepContext<'a> {
    types: &'a TypeRegistry,
    epoch: CollectionEpoch,
    promoted_active_old: &'a mut [Option<SpanId>; SMALL_SIZE_CLASSES.len()],
    eden_pool: &'a mut EdenPool,
    external_bytes: &'a mut usize,
    stats: &'a mut MinorSweepStats,
}

/// Sweeps only the intrusive Eden/Survivor chain and transitions every retained whole span.
pub(crate) fn sweep_young(
    table: &mut SpanTable,
    types: &TypeRegistry,
    epoch: CollectionEpoch,
    promoted_active_old: &mut [Option<SpanId>; SMALL_SIZE_CLASSES.len()],
    eden_pool: &mut EdenPool,
    external_bytes: &mut usize,
    stats: &mut MinorSweepStats,
) -> Result<(), SweepError> {
    let mut context = YoungSweepContext {
        types,
        epoch,
        promoted_active_old,
        eden_pool,
        external_bytes,
        stats,
    };
    let mut current = table.young_head();
    while let Some(span_id) = current {
        let next = table.young_next(span_id);
        if let Some(space) = table.young_space(span_id) {
            context.stats.sweep.spans_processed += 1;
            match space {
                SpanSpace::Eden => context.stats.eden_spans_processed += 1,
                SpanSpace::Survivor { .. } => context.stats.survivor_spans_processed += 1,
                SpanSpace::Old => unreachable!("young_space excludes Old"),
            }
            sweep_young_span(table, span_id, &mut context)?;
        }
        current = next;
    }
    table.compact_young_spans();
    context.stats.eden_pool_retained_bytes = context.eden_pool.stats().retained_bytes;
    Ok(())
}

/// Drops white young slots, releases empty storage, then ages or promotes the retained span.
fn sweep_young_span(
    table: &mut SpanTable,
    span_id: SpanId,
    context: &mut YoungSweepContext<'_>,
) -> Result<(), SweepError> {
    let before = table
        .small_sweep_snapshot(span_id)
        .expect("young chain contains a live small span");
    let mut cursor = 0;
    let mut live_slots = 0_u16;
    while let Some(reference) = table.next_allocated_reference(span_id, cursor) {
        cursor = reference_slot_successor(before, reference);
        if table
            .is_marked(reference, context.epoch)
            .map_err(SweepError::InvalidReference)?
        {
            live_slots += 1;
        }
    }
    let class_index = SMALL_SIZE_CLASSES
        .iter()
        .position(|&size| size == before.size_class.slot_size())
        .expect("every allocated small span uses a tuning size class");
    if live_slots == 0 && !context.eden_pool.has_capacity(class_index) {
        table
            .prepare_release(span_id)
            .map_err(SweepError::SpanTable)?;
    }
    sweep_young_objects(
        table,
        context.types,
        span_id,
        context.epoch,
        before,
        context.external_bytes,
        context.stats,
    )?;
    if live_slots == 0 {
        if context.eden_pool.retain(class_index, span_id) {
            table.pool_empty_young(span_id);
            context.stats.eden_spans_pooled += 1;
            return Ok(());
        }
        table.release(span_id).map_err(SweepError::SpanTable)?;
        context.stats.sweep.spans_released += 1;
        context.stats.sweep.released_storage_bytes += SPAN_SIZE_BYTES;
        context.stats.eden_pool_overflow_spans_released += 1;
        return Ok(());
    }
    let due_by_age =
        crate::tuning::promotion_due_to_age(before.space, crate::tuning::YOUNG_PROMOTION_AGE);
    let promote_early = !due_by_age
        && crate::tuning::should_promote_early(live_slots, before.size_class.slot_count());
    let transition =
        table.advance_young_cohort(span_id, crate::tuning::YOUNG_PROMOTION_AGE, promote_early);
    match transition {
        YoungSpanTransition::EdenToSurvivor => context.stats.eden_to_survivor += 1,
        YoungSpanTransition::SurvivorAged => context.stats.survivor_spans_aged += 1,
        YoungSpanTransition::Promoted => {
            context.stats.whole_span_promotions += 1;
            context.stats.early_whole_span_promotions += usize::from(promote_early);
            table.rebuild_old_free_list(span_id);
            if table.can_allocate_in_span(span_id) {
                context.promoted_active_old[class_index] = Some(span_id);
            }
        }
    }
    let after = table
        .small_sweep_snapshot(span_id)
        .expect("retained young span remains installed after transition");
    debug_assert_eq!(after.allocated_slots, live_slots);
    account_retained_small(after, &mut context.stats.sweep);
    Ok(())
}

/// Performs the destructive pass after release capacity and live counts have been preflighted.
fn sweep_young_objects(
    table: &mut SpanTable,
    types: &TypeRegistry,
    span_id: SpanId,
    epoch: CollectionEpoch,
    snapshot: SmallSweepSnapshot,
    external_bytes: &mut usize,
    stats: &mut MinorSweepStats,
) -> Result<(), SweepError> {
    let mut cursor = 0;
    while let Some(reference) = table.next_allocated_reference(span_id, cursor) {
        cursor = reference_slot_successor(snapshot, reference);
        stats.sweep.scanned_objects += 1;
        stats.sweep.scanned_bytes += usize::from(snapshot.size_class.slot_size());
        if table
            .is_marked(reference, epoch)
            .map_err(SweepError::InvalidReference)?
        {
            stats.sweep.live_objects += 1;
            stats.sweep.live_bytes += usize::from(snapshot.size_class.slot_size());
        } else {
            drop_small(table, types, reference, external_bytes)?;
            stats.sweep.reclaimed_objects += 1;
            stats.sweep.reclaimed_bytes += usize::from(snapshot.size_class.slot_size());
        }
    }
    Ok(())
}

/// Preflights whole-span release, then drops each white payload in allocation-bit order.
fn sweep_small(
    table: &mut SpanTable,
    types: &TypeRegistry,
    span_id: SpanId,
    epoch: CollectionEpoch,
    external_bytes: &mut usize,
    stats: &mut SweepStats,
) -> Result<(), SweepError> {
    let before = table
        .small_sweep_snapshot(span_id)
        .expect("sweep worklist contains a live small owner");
    let mut cursor = 0;
    let mut live_slots = 0_u16;
    while let Some(reference) = table.next_allocated_reference(span_id, cursor) {
        cursor = reference_slot_successor(before, reference);
        if table
            .is_marked(reference, epoch)
            .map_err(SweepError::InvalidReference)?
        {
            live_slots += 1;
        }
    }
    if live_slots == 0 {
        table
            .prepare_release(span_id)
            .map_err(SweepError::SpanTable)?;
    }

    cursor = 0;
    while let Some(reference) = table.next_allocated_reference(span_id, cursor) {
        cursor = reference_slot_successor(before, reference);
        stats.scanned_objects += 1;
        stats.scanned_bytes += usize::from(before.size_class.slot_size());
        if table
            .is_marked(reference, epoch)
            .map_err(SweepError::InvalidReference)?
        {
            stats.live_objects += 1;
            stats.live_bytes += usize::from(before.size_class.slot_size());
            continue;
        }
        drop_small(table, types, reference, external_bytes)?;
        stats.reclaimed_objects += 1;
        stats.reclaimed_bytes += usize::from(before.size_class.slot_size());
    }

    let after = table
        .small_sweep_snapshot(span_id)
        .expect("small span remains installed until empty-span release");
    debug_assert_eq!(after.allocated_slots, live_slots);
    if after.allocated_slots == 0 {
        table.release(span_id).map_err(SweepError::SpanTable)?;
        stats.spans_released += 1;
        stats.released_storage_bytes += SPAN_SIZE_BYTES;
        return Ok(());
    }
    table.rebuild_old_free_list(span_id);
    account_retained_small(after, stats);
    Ok(())
}

/// Resolves the concrete callback before unpublishing, then makes panic retry double-drop safe.
fn drop_small(
    table: &mut SpanTable,
    types: &TypeRegistry,
    reference: RawHeapRef,
    external_bytes: &mut usize,
) -> Result<(), SweepError> {
    let header = table
        .verify_reference(reference, None)
        .map_err(SweepError::InvalidReference)?;
    let type_id = header
        .type_id()
        .ok_or(SweepError::UnknownTypeId(reference))?;
    let descriptor = types
        .descriptor(type_id)
        .ok_or(SweepError::UnknownTypeId(reference))?;
    let payload = table
        .payload_address(reference, descriptor)
        .map_err(SweepError::InvalidReference)?;
    let charged = header.external_bytes().unwrap_or(0);
    ensure_external_charge_available(reference, charged, *external_bytes)?;
    if !table.unpublish_small_for_drop(reference) {
        return Err(SweepError::AllocationStateChanged(reference));
    }
    *external_bytes -= charged;
    // SAFETY: descriptor/header identity and payload placement were revalidated immediately above;
    // unpublishing changed only side metadata, backing storage remains installed and exclusively
    // owned by this single-mutator table, and a panic cannot republish or double-drop this slot.
    unsafe { descriptor.drop(payload) };
    Ok(())
}

/// Drops or retains one large owner and always reclaims its complete continuation range together.
fn sweep_large(
    table: &mut SpanTable,
    types: &TypeRegistry,
    span_id: SpanId,
    epoch: CollectionEpoch,
    external_bytes: &mut usize,
    stats: &mut SweepStats,
) -> Result<(), SweepError> {
    let metadata = table
        .large_metadata(span_id)
        .expect("sweep worklist contains a live large owner");
    let reference = RawHeapRef::from_parts(
        span_id,
        crate::SpanOffset::new(MINIMUM_SLOT_SIZE_BYTES as u16)
            .expect("large owner offset is non-zero"),
    );
    if !metadata.is_allocated() {
        table
            .prepare_release(span_id)
            .map_err(SweepError::SpanTable)?;
        let reclaimed = table
            .release_unpublished_large(reference)
            .map_err(SweepError::SpanTable)?;
        stats.spans_released += reclaimed.span_count() as usize;
        stats.released_storage_bytes += reclaimed.storage_bytes();
        return Ok(());
    }
    stats.scanned_objects += 1;
    stats.scanned_bytes += metadata.object_bytes();
    if table
        .is_marked(reference, epoch)
        .map_err(SweepError::InvalidReference)?
    {
        stats.live_objects += 1;
        stats.live_bytes += metadata.object_bytes();
        let storage = metadata.span_count() as usize * SPAN_SIZE_BYTES;
        stats.retained_tail_slack_bytes +=
            storage.saturating_sub(MINIMUM_SLOT_SIZE_BYTES + metadata.object_bytes());
        return Ok(());
    }

    table
        .prepare_release(span_id)
        .map_err(SweepError::SpanTable)?;
    let header = table
        .verify_reference(reference, None)
        .map_err(SweepError::InvalidReference)?;
    let type_id = header
        .type_id()
        .ok_or(SweepError::UnknownTypeId(reference))?;
    let descriptor = types
        .descriptor(type_id)
        .ok_or(SweepError::UnknownTypeId(reference))?;
    let payload = table
        .payload_address(reference, descriptor)
        .map_err(SweepError::InvalidReference)?;
    let charged = header.external_bytes().unwrap_or(0);
    ensure_external_charge_available(reference, charged, *external_bytes)?;
    table
        .unpublish_large_for_drop(reference)
        .map_err(SweepError::SpanTable)?;
    *external_bytes -= charged;
    // SAFETY: the verified owner header selects this immutable descriptor and payload layout;
    // unpublishing retains the whole owner/continuation backing range until callback completion.
    unsafe { descriptor.drop(payload) };
    let reclaimed = table
        .release_unpublished_large(reference)
        .map_err(SweepError::SpanTable)?;
    stats.reclaimed_objects += 1;
    stats.reclaimed_bytes += reclaimed.object_bytes();
    stats.spans_released += reclaimed.span_count() as usize;
    stats.released_storage_bytes += reclaimed.storage_bytes();
    Ok(())
}

fn ensure_external_charge_available(
    reference: RawHeapRef,
    charged: usize,
    available: usize,
) -> Result<(), SweepError> {
    if charged > available {
        return Err(SweepError::ExternalAccountingUnderflow {
            reference,
            charged,
            available,
        });
    }
    Ok(())
}

#[inline(always)]
fn reference_slot_successor(snapshot: SmallSweepSnapshot, reference: RawHeapRef) -> u16 {
    snapshot
        .size_class
        .slot_for_offset(reference.span_offset())
        .expect("allocation iterator returns aligned references")
        .index()
        + 1
}

fn account_retained_small(snapshot: SmallSweepSnapshot, stats: &mut SweepStats) {
    let slot_size = usize::from(snapshot.size_class.slot_size());
    let initialized_region = usize::from(snapshot.bump_cursor) * slot_size;
    let allocated = snapshot.allocated_bytes as usize;
    let fragmentation = initialized_region.saturating_sub(allocated);
    stats.retained_fragmentation_bytes += fragmentation;
    if snapshot.space == SpanSpace::Old {
        stats.reusable_old_free_bytes += fragmentation;
    }
    let usable = SPAN_SIZE_BYTES - MINIMUM_SLOT_SIZE_BYTES;
    stats.retained_tail_slack_bytes += usable.saturating_sub(initialized_region);
}

#[cfg(test)]
mod tests {
    use super::{SweepWorklist, SweepWorklistError};
    use crate::{MAX_LOGICAL_SPANS, SpanId};

    #[test]
    fn span_worklist_growth_retains_high_water_and_enforces_quota() {
        let limit = 100;
        let mut worklist = SweepWorklist::new(limit);
        for index in 0..limit {
            worklist.try_push(SpanId::new(index as u16)).unwrap();
        }
        assert_eq!(
            worklist.try_push(SpanId::new(limit as u16)),
            Err(SweepWorklistError::EntryLimitExceeded { limit })
        );
        let stats = worklist.stats();
        assert_eq!(stats.initial_capacity, 64);
        assert_eq!(stats.growth_count, 2);
        assert_eq!(stats.peak_len, limit);
        assert_eq!(stats.retained_capacity, limit);
        worklist.clear();
        assert_eq!(worklist.stats().slack_entries, limit);
    }

    #[test]
    fn logical_span_limit_fits_the_worklist_index_type() {
        assert_eq!(MAX_LOGICAL_SPANS, usize::from(u16::MAX) + 1);
    }
}
