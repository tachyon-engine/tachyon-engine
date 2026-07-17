//! Iterative strong marking over exact roots and descriptor-provided object edges.

use crate::{
    CardBitmap, CollectionEpoch, FinalizationQueueError, GrayQueueError, HeapReferenceError,
    RawHeapRef, SpanId, SpanTable, Trace, Tracer, TypeDescriptor, TypeRegistry,
    finalization::PendingFinalizations,
    gray::GrayQueue,
    table::ReferenceSpace,
    weak::{WeakOwner, WeakOwnerError, WeakOwners},
};
use tachyon_value::Value;

/// A strong-mark failure leaves partial bits in the abandoned epoch but no queued work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkError {
    InvalidReference(HeapReferenceError),
    GrayQueue(GrayQueueError),
    WeakOwners(WeakOwnerError),
    FinalizationQueue(FinalizationQueueError),
    UnknownTypeId(RawHeapRef),
}

/// Deterministic work counters for collection tests and later pause budgeting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkStats {
    pub traced_edges: usize,
    pub marked_objects: usize,
    pub traced_objects: usize,
    pub weak_owners: usize,
    pub ephemeron_passes: usize,
    pub ephemeron_values_marked: usize,
    pub weak_slots_cleared: usize,
    pub ephemerons_cleared: usize,
    pub finalizations_enqueued: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TracePhase {
    Strong,
    Ephemeron,
    ClearWeak,
    EnqueueFinalization,
}

/// Deterministic young-mark and remembered-set work counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YoungMarkStats {
    pub mark: MarkStats,
    pub dirty_cards_scanned: usize,
    pub old_objects_scanned: usize,
    pub remembered_large_owners_scanned: usize,
    pub promotion_objects_scanned: usize,
    pub card_false_positive_cards: usize,
}

/// Reaches the strong fixed point without recursively tracing through the native stack.
pub(crate) fn mark_strong_roots(
    table: &mut SpanTable,
    types: &TypeRegistry,
    gray: &mut GrayQueue,
    weak_owners: &mut WeakOwners,
    pending_finalizations: &mut PendingFinalizations,
    epoch: CollectionEpoch,
    roots: &mut dyn Trace,
) -> Result<MarkStats, MarkError> {
    gray.clear();
    weak_owners.clear();
    let mut marker = Marker {
        table,
        gray,
        weak_owners,
        pending_finalizations,
        epoch,
        error: None,
        stats: MarkStats::default(),
        current_has_weak: false,
        current_has_ephemeron: false,
        current_has_finalization: false,
        current_owner: None,
        phase: TracePhase::Strong,
    };
    roots.trace(&mut marker);
    marker.mark_pending_finalization_roots();
    marker.drain(types);
    marker.close_weak(types);
    if let Some(error) = marker.error {
        marker.gray.clear();
        return Err(error);
    }
    Ok(marker.stats)
}

/// Marks only Eden/Survivor reachability from exact roots and remembered Old sources.
pub(crate) fn mark_young_roots(
    table: &mut SpanTable,
    types: &TypeRegistry,
    gray: &mut GrayQueue,
    weak_owners: &mut WeakOwners,
    pending_finalizations: &mut PendingFinalizations,
    epoch: CollectionEpoch,
    roots: &mut dyn Trace,
) -> Result<YoungMarkStats, MarkError> {
    gray.clear();
    weak_owners.clear();
    let mut marker = YoungMarker {
        table,
        gray,
        weak_owners,
        pending_finalizations,
        epoch,
        error: None,
        stats: YoungMarkStats::default(),
        rebuilding_source: false,
        source_has_young: false,
        current_has_weak: false,
        current_has_ephemeron: false,
        current_has_finalization: false,
        current_owner: None,
        phase: TracePhase::Strong,
    };
    roots.trace(&mut marker);
    marker.mark_pending_finalization_roots();
    marker.scan_remembered(types);
    marker.drain(types);
    marker.close_weak(types);
    if let Some(error) = marker.error {
        marker.gray.clear();
        return Err(error);
    }
    marker.rebuild_remembered(types);
    marker.prepare_promotions(types);
    Ok(marker.stats)
}

struct Marker<'a> {
    table: &'a mut SpanTable,
    gray: &'a mut GrayQueue,
    weak_owners: &'a mut WeakOwners,
    pending_finalizations: &'a mut PendingFinalizations,
    epoch: CollectionEpoch,
    error: Option<MarkError>,
    stats: MarkStats,
    current_has_weak: bool,
    current_has_ephemeron: bool,
    current_has_finalization: bool,
    current_owner: Option<RawHeapRef>,
    phase: TracePhase,
}

impl Marker<'_> {
    #[inline(always)]
    fn enqueue(&mut self, reference: RawHeapRef) {
        if self.error.is_some() {
            return;
        }
        self.stats.traced_edges += 1;
        let already_marked = match self.table.is_marked(reference, self.epoch) {
            Ok(marked) => marked,
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return;
            }
        };
        if already_marked {
            return;
        }
        if let Err(error) = self.gray.try_reserve_one() {
            self.error = Some(MarkError::GrayQueue(error));
            return;
        }
        match self.table.mark_reference(reference, self.epoch) {
            Ok(true) => {
                self.gray.push_reserved(reference);
                self.stats.marked_objects += 1;
            }
            Ok(false) => {}
            Err(error) => self.error = Some(MarkError::InvalidReference(error)),
        }
    }

    /// Pops one object at a time; temporary payload pointers never survive a trace callback.
    fn drain(&mut self, types: &TypeRegistry) {
        while self.error.is_none() {
            let Some(reference) = self.gray.pop() else {
                break;
            };
            let header = match self.table.verify_reference(reference, None) {
                Ok(header) => header,
                Err(error) => {
                    self.error = Some(MarkError::InvalidReference(error));
                    break;
                }
            };
            let Some(type_id) = header.type_id() else {
                self.error = Some(MarkError::UnknownTypeId(reference));
                break;
            };
            let Some(descriptor) = types.descriptor(type_id) else {
                self.error = Some(MarkError::UnknownTypeId(reference));
                break;
            };
            let payload = match self.table.payload_address(reference, descriptor) {
                Ok(payload) => payload,
                Err(error) => {
                    self.error = Some(MarkError::InvalidReference(error));
                    break;
                }
            };
            self.stats.traced_objects += 1;
            self.current_has_weak = false;
            self.current_has_ephemeron = false;
            self.current_has_finalization = false;
            // SAFETY: table verification matched the live header ID to this immutable descriptor;
            // storage is independently allocated and cannot move while this marker only changes
            // side metadata and its separate gray queue. No other payload borrow is active.
            unsafe { descriptor.trace(payload, self) };
            self.record_weak_owner(reference);
        }
    }

    /// Publishes at most one weak-phase entry for the object just traced strongly.
    fn record_weak_owner(&mut self, reference: RawHeapRef) {
        if !self.current_has_weak && !self.current_has_ephemeron && !self.current_has_finalization {
            return;
        }
        if let Err(error) = self.weak_owners.try_push(WeakOwner {
            reference,
            has_weak: self.current_has_weak,
            has_ephemeron: self.current_has_ephemeron,
            has_finalization: self.current_has_finalization,
        }) {
            self.error = Some(MarkError::WeakOwners(error));
        } else {
            self.stats.weak_owners += 1;
        }
    }

    /// Reaches ephemeron closure, then clears weak slots before any sweep can reclaim targets.
    fn close_weak(&mut self, types: &TypeRegistry) {
        while self.error.is_none() {
            let owners_before = self.weak_owners.len();
            let marks_before = self.stats.marked_objects;
            let mut found_ephemeron = false;
            self.phase = TracePhase::Ephemeron;
            for index in 0..owners_before {
                let owner = self
                    .weak_owners
                    .get(index)
                    .expect("closure snapshot indices remain stable");
                if owner.has_ephemeron {
                    found_ephemeron = true;
                    self.trace_weak_owner(owner.reference, types);
                }
            }
            if !found_ephemeron || self.error.is_some() {
                break;
            }
            self.stats.ephemeron_passes += 1;
            self.phase = TracePhase::Strong;
            self.drain(types);
            if self.stats.marked_objects == marks_before && self.weak_owners.len() == owners_before
            {
                break;
            }
        }
        if self.error.is_some() {
            return;
        }
        self.phase = TracePhase::ClearWeak;
        for index in 0..self.weak_owners.len() {
            let owner = self
                .weak_owners
                .get(index)
                .expect("weak clearing uses a stable completed worklist");
            self.trace_weak_owner(owner.reference, types);
            if self.error.is_some() {
                break;
            }
        }
        if self.error.is_none() {
            self.phase = TracePhase::EnqueueFinalization;
            for index in 0..self.weak_owners.len() {
                let owner = self
                    .weak_owners
                    .get(index)
                    .expect("finalization enqueue uses a stable worklist");
                if owner.has_finalization {
                    self.trace_weak_owner(owner.reference, types);
                }
                if self.error.is_some() {
                    break;
                }
            }
        }
        self.phase = TracePhase::Strong;
    }

    /// Marks cleanup records retained from earlier collections before discovering new garbage.
    fn mark_pending_finalization_roots(&mut self) {
        for index in 0..self.pending_finalizations.len() {
            let record = self
                .pending_finalizations
                .get(index)
                .expect("pending finalization indices remain stable during root marking");
            self.enqueue(record.registry());
            if let Some(held) = record.held_value().as_heap_ref() {
                self.enqueue(held);
            }
        }
    }

    /// Resolves a registered owner afresh for each weak phase without retaining payload pointers.
    fn trace_weak_owner(&mut self, reference: RawHeapRef, types: &TypeRegistry) {
        let header = match self.table.verify_reference(reference, None) {
            Ok(header) => header,
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return;
            }
        };
        let Some(type_id) = header.type_id() else {
            self.error = Some(MarkError::UnknownTypeId(reference));
            return;
        };
        let Some(descriptor) = types.descriptor(type_id) else {
            self.error = Some(MarkError::UnknownTypeId(reference));
            return;
        };
        let payload = match self.table.payload_address(reference, descriptor) {
            Ok(payload) => payload,
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return;
            }
        };
        // SAFETY: the owner remains marked and allocated until weak closure completes; descriptor
        // identity is revalidated for every phase and the callback cannot retain this pointer.
        self.current_owner = Some(reference);
        unsafe { descriptor.trace(payload, self) };
        self.current_owner = None;
    }

    #[inline(always)]
    fn reference_live(&mut self, reference: RawHeapRef) -> bool {
        match self.table.is_marked(reference, self.epoch) {
            Ok(live) => live,
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                false
            }
        }
    }
}

impl Tracer for Marker<'_> {
    #[inline(always)]
    fn trace_value(&mut self, value: &mut Value) {
        if matches!(self.phase, TracePhase::Strong | TracePhase::Ephemeron)
            && let Some(reference) = value.as_heap_ref()
        {
            self.enqueue(reference);
        }
    }

    #[inline(always)]
    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
        if matches!(self.phase, TracePhase::Strong | TracePhase::Ephemeron) {
            self.enqueue(*reference);
        }
    }

    #[inline(always)]
    fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
        match self.phase {
            TracePhase::Strong => self.current_has_weak |= reference.is_some(),
            TracePhase::Ephemeron => {}
            TracePhase::EnqueueFinalization => {}
            TracePhase::ClearWeak => {
                if let Some(target) = *reference {
                    let live = self.reference_live(target);
                    if self.error.is_none() && !live {
                        *reference = None;
                        self.stats.weak_slots_cleared += 1;
                    }
                }
            }
        }
    }

    #[inline(always)]
    fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, value: &mut Value) {
        match self.phase {
            TracePhase::Strong => self.current_has_ephemeron |= key.is_some(),
            TracePhase::Ephemeron => {
                if key.is_some_and(|key| self.reference_live(key))
                    && let Some(value) = value.as_heap_ref()
                {
                    let before = self.stats.marked_objects;
                    self.enqueue(value);
                    self.stats.ephemeron_values_marked += self.stats.marked_objects - before;
                }
            }
            TracePhase::ClearWeak => {
                let live = key.is_some_and(|key| self.reference_live(key));
                if self.error.is_none() && !live {
                    *key = None;
                    *value = Value::from_immediate(tachyon_value::Immediate::Undefined);
                    self.stats.ephemerons_cleared += 1;
                }
            }
            TracePhase::EnqueueFinalization => {}
        }
    }

    #[inline(always)]
    fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, held_value: &mut Value) {
        match self.phase {
            TracePhase::Strong => {
                self.current_has_finalization |= target.is_some();
                self.trace_value(held_value);
            }
            TracePhase::Ephemeron | TracePhase::ClearWeak => {}
            TracePhase::EnqueueFinalization => {
                let Some(target_reference) = *target else {
                    return;
                };
                let live = self.reference_live(target_reference);
                if self.error.is_some() || live {
                    return;
                }
                let registry = self
                    .current_owner
                    .expect("finalization callbacks run while resolving one owner");
                match self
                    .pending_finalizations
                    .try_enqueue(registry, *held_value)
                {
                    Ok(()) => {
                        *target = None;
                        *held_value = Value::from_immediate(tachyon_value::Immediate::Undefined);
                        self.stats.finalizations_enqueued += 1;
                    }
                    Err(error) => self.error = Some(MarkError::FinalizationQueue(error)),
                }
            }
        }
    }
}

struct YoungMarker<'a> {
    table: &'a mut SpanTable,
    gray: &'a mut GrayQueue,
    weak_owners: &'a mut WeakOwners,
    pending_finalizations: &'a mut PendingFinalizations,
    epoch: CollectionEpoch,
    error: Option<MarkError>,
    stats: YoungMarkStats,
    rebuilding_source: bool,
    source_has_young: bool,
    current_has_weak: bool,
    current_has_ephemeron: bool,
    current_has_finalization: bool,
    current_owner: Option<RawHeapRef>,
    phase: TracePhase,
}

impl YoungMarker<'_> {
    /// Classifies every edge before marking so roots and young objects never recurse through Old.
    #[inline(always)]
    fn enqueue(&mut self, reference: RawHeapRef) {
        if self.error.is_some() && !self.rebuilding_source {
            return;
        }
        let space = match self.table.reference_space(reference) {
            Ok(space) => space,
            Err(error) if self.rebuilding_source => {
                self.source_has_young = true;
                let _ = error;
                return;
            }
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return;
            }
        };
        if self.rebuilding_source {
            self.source_has_young |= space == ReferenceSpace::Young;
            return;
        }
        self.stats.mark.traced_edges += 1;
        if space != ReferenceSpace::Young {
            return;
        }
        let already_marked = match self.table.is_marked(reference, self.epoch) {
            Ok(marked) => marked,
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return;
            }
        };
        if already_marked {
            return;
        }
        if let Err(error) = self.gray.try_reserve_one() {
            self.error = Some(MarkError::GrayQueue(error));
            return;
        }
        match self.table.mark_reference(reference, self.epoch) {
            Ok(true) => {
                self.gray.push_reserved(reference);
                self.stats.mark.marked_objects += 1;
            }
            Ok(false) => {}
            Err(error) => self.error = Some(MarkError::InvalidReference(error)),
        }
    }

    /// Scans only dirty Old cards and remembered large owners, leaving metadata unchanged on error.
    fn scan_remembered(&mut self, types: &TypeRegistry) {
        let mut current = self.table.remembered_head();
        while let Some(span_id) = current {
            let next = self.table.remembered_next(span_id);
            if self.error.is_some() {
                return;
            }
            let dirty_cards = self.table.dirty_old_card_count(span_id);
            if dirty_cards != 0 {
                self.stats.dirty_cards_scanned += dirty_cards;
                self.scan_dirty_small_span(span_id, types);
            } else if let Some(reference) = self.table.remembered_large_reference(span_id) {
                self.stats.remembered_large_owners_scanned += 1;
                self.trace_source(reference, types, false);
            }
            current = next;
        }
    }

    /// Walks allocation bits but invokes descriptors only for objects beginning on dirty cards.
    fn scan_dirty_small_span(&mut self, span_id: SpanId, types: &TypeRegistry) {
        let mut start = 0;
        while self.error.is_none() {
            let Some(reference) = self.table.next_dirty_old_reference(span_id, start) else {
                break;
            };
            self.stats.old_objects_scanned += 1;
            self.trace_source(reference, types, false);
            let Some(next) = self.next_slot(reference) else {
                break;
            };
            start = next;
        }
    }

    /// Drains only marked young objects; their Old outgoing edges are classified then ignored.
    fn drain(&mut self, types: &TypeRegistry) {
        while self.error.is_none() {
            let Some(reference) = self.gray.pop() else {
                break;
            };
            self.stats.mark.traced_objects += 1;
            self.trace_source(reference, types, false);
        }
    }

    /// Resolves one descriptor and invokes its exact trace callback without retaining payload borrows.
    fn trace_source(
        &mut self,
        reference: RawHeapRef,
        types: &TypeRegistry,
        rebuilding: bool,
    ) -> bool {
        let descriptor = match self.descriptor(reference, types) {
            Some(descriptor) => descriptor,
            None if rebuilding => {
                self.error = None;
                return true;
            }
            None => return false,
        };
        let payload = match self.table.payload_address(reference, descriptor) {
            Ok(payload) => payload,
            Err(error) if rebuilding => {
                let _ = error;
                return true;
            }
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return false;
            }
        };
        self.rebuilding_source = rebuilding;
        self.source_has_young = false;
        self.current_has_weak = false;
        self.current_has_ephemeron = false;
        self.current_has_finalization = false;
        // SAFETY: exact header/descriptor validation owns the same boundary as full marking. The
        // table never reallocates while tracing; only side metadata and the separate queue mutate.
        unsafe { descriptor.trace(payload, self) };
        self.rebuilding_source = false;
        if !rebuilding {
            self.record_weak_owner(reference);
        }
        self.source_has_young
    }

    /// Publishes one young/minor weak owner without retaining payload addresses.
    fn record_weak_owner(&mut self, reference: RawHeapRef) {
        if !self.current_has_weak && !self.current_has_ephemeron && !self.current_has_finalization {
            return;
        }
        if let Err(error) = self.weak_owners.try_push(WeakOwner {
            reference,
            has_weak: self.current_has_weak,
            has_ephemeron: self.current_has_ephemeron,
            has_finalization: self.current_has_finalization,
        }) {
            self.error = Some(MarkError::WeakOwners(error));
        } else {
            self.stats.mark.weak_owners += 1;
        }
    }

    /// Applies the same weak phase order as full major while treating every Old target as live.
    fn close_weak(&mut self, types: &TypeRegistry) {
        while self.error.is_none() {
            let owners_before = self.weak_owners.len();
            let marks_before = self.stats.mark.marked_objects;
            let mut found_ephemeron = false;
            self.phase = TracePhase::Ephemeron;
            for index in 0..owners_before {
                let owner = self
                    .weak_owners
                    .get(index)
                    .expect("minor closure snapshot indices remain stable");
                if owner.has_ephemeron {
                    found_ephemeron = true;
                    self.trace_weak_owner(owner.reference, types);
                }
            }
            if !found_ephemeron || self.error.is_some() {
                break;
            }
            self.stats.mark.ephemeron_passes += 1;
            self.phase = TracePhase::Strong;
            self.drain(types);
            if self.stats.mark.marked_objects == marks_before
                && self.weak_owners.len() == owners_before
            {
                break;
            }
        }
        if self.error.is_some() {
            return;
        }
        self.phase = TracePhase::ClearWeak;
        for index in 0..self.weak_owners.len() {
            let owner = self
                .weak_owners
                .get(index)
                .expect("minor weak clearing uses a stable worklist");
            self.trace_weak_owner(owner.reference, types);
            if self.error.is_some() {
                break;
            }
        }
        if self.error.is_none() {
            self.phase = TracePhase::EnqueueFinalization;
            for index in 0..self.weak_owners.len() {
                let owner = self
                    .weak_owners
                    .get(index)
                    .expect("minor finalization enqueue uses a stable worklist");
                if owner.has_finalization {
                    self.trace_weak_owner(owner.reference, types);
                }
                if self.error.is_some() {
                    break;
                }
            }
        }
        self.phase = TracePhase::Strong;
    }

    /// Marks pending registry/held-value records under young liveness rules.
    fn mark_pending_finalization_roots(&mut self) {
        for index in 0..self.pending_finalizations.len() {
            let record = self
                .pending_finalizations
                .get(index)
                .expect("pending finalization indices remain stable during minor roots");
            self.enqueue(record.registry());
            if let Some(held) = record.held_value().as_heap_ref() {
                self.enqueue(held);
            }
        }
    }

    /// Re-resolves one young or remembered Old owner before invoking a weak-phase trace.
    fn trace_weak_owner(&mut self, reference: RawHeapRef, types: &TypeRegistry) {
        let Some(descriptor) = self.descriptor(reference, types) else {
            return;
        };
        let payload = match self.table.payload_address(reference, descriptor) {
            Ok(payload) => payload,
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return;
            }
        };
        // SAFETY: weak owners remain allocated throughout closure; descriptor identity and payload
        // placement are revalidated and no payload pointer survives this callback.
        self.current_owner = Some(reference);
        unsafe { descriptor.trace(payload, self) };
        self.current_owner = None;
    }

    #[inline(always)]
    fn reference_live(&mut self, reference: RawHeapRef) -> bool {
        match self.table.reference_space(reference) {
            Ok(ReferenceSpace::OldSmall | ReferenceSpace::OldLarge) => true,
            Ok(ReferenceSpace::Young) => match self.table.is_marked(reference, self.epoch) {
                Ok(live) => live,
                Err(error) => {
                    self.error = Some(MarkError::InvalidReference(error));
                    false
                }
            },
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                false
            }
        }
    }

    /// Resolves one immutable descriptor while translating every malformed source into mark error.
    fn descriptor(
        &mut self,
        reference: RawHeapRef,
        types: &TypeRegistry,
    ) -> Option<TypeDescriptor> {
        let header = match self.table.verify_reference(reference, None) {
            Ok(header) => header,
            Err(error) => {
                self.error = Some(MarkError::InvalidReference(error));
                return None;
            }
        };
        let Some(type_id) = header.type_id() else {
            self.error = Some(MarkError::UnknownTypeId(reference));
            return None;
        };
        let descriptor = types.descriptor(type_id);
        if descriptor.is_none() {
            self.error = Some(MarkError::UnknownTypeId(reference));
        }
        descriptor
    }

    /// Rebuilds only metadata that was scanned successfully; invalid late edges stay conservative.
    fn rebuild_remembered(&mut self, types: &TypeRegistry) {
        let mut current = self.table.remembered_head();
        while let Some(span_id) = current {
            let next = self.table.remembered_next(span_id);
            if self.table.dirty_old_card_count(span_id) != 0 {
                self.rebuild_small_span(span_id, types);
            } else if let Some(reference) = self.table.remembered_large_reference(span_id) {
                let remembered = self.trace_source(reference, types, true);
                self.table.set_large_remembered(span_id, remembered);
            }
            current = next;
        }
        self.table.compact_remembered_sources();
    }

    fn rebuild_small_span(&mut self, span_id: SpanId, types: &TypeRegistry) {
        let dirty_before = self.table.dirty_old_card_count(span_id);
        let mut rebuilt = CardBitmap::new();
        let mut start = 0;
        while let Some(reference) = self.table.next_dirty_old_reference(span_id, start) {
            if self.trace_source(reference, types, true) {
                rebuilt.mark(reference.span_offset());
            }
            let Some(next) = self.next_slot(reference) else {
                break;
            };
            start = next;
        }
        let dirty_after = rebuilt.dirty_count();
        self.stats.card_false_positive_cards += dirty_before.saturating_sub(dirty_after);
        self.table.replace_old_cards(span_id, rebuilt);
    }

    /// Builds exact cards from marked survivors immediately before their possible promotion.
    fn prepare_promotions(&mut self, types: &TypeRegistry) {
        let promotion_age = crate::tuning::YOUNG_PROMOTION_AGE;
        let mut current = self.table.young_head();
        while let Some(span_id) = current {
            let next = self.table.young_next(span_id);
            if self.table.young_space(span_id).is_some_and(|space| {
                matches!(space, crate::SpanSpace::Survivor { age } if age.saturating_add(1) >= promotion_age)
            }) {
                self.prepare_promotion_span(span_id, types);
            }
            current = next;
        }
    }

    /// Scans only live objects in one candidate and records cards containing direct young sources.
    fn prepare_promotion_span(&mut self, span_id: SpanId, types: &TypeRegistry) {
        let mut cards = CardBitmap::new();
        let mut start = 0;
        while let Some(reference) = self.table.next_allocated_reference(span_id, start) {
            let Some(next) = self.next_slot(reference) else {
                break;
            };
            start = next;
            if !self.table.is_marked(reference, self.epoch).unwrap_or(false) {
                continue;
            }
            self.stats.promotion_objects_scanned += 1;
            if self.trace_source(reference, types, true) {
                cards.mark(reference.span_offset());
            }
        }
        self.table.replace_promotion_cards(span_id, cards);
    }

    fn next_slot(&self, reference: RawHeapRef) -> Option<u16> {
        self.table
            .metadata(reference.span_id())?
            .size_class()
            .slot_for_offset(reference.span_offset())?
            .index()
            .checked_add(1)
    }
}

impl Tracer for YoungMarker<'_> {
    #[inline(always)]
    fn trace_value(&mut self, value: &mut Value) {
        if matches!(self.phase, TracePhase::Strong | TracePhase::Ephemeron)
            && let Some(reference) = value.as_heap_ref()
        {
            self.enqueue(reference);
        }
    }

    #[inline(always)]
    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
        if matches!(self.phase, TracePhase::Strong | TracePhase::Ephemeron) {
            self.enqueue(*reference);
        }
    }

    #[inline(always)]
    fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
        if self.rebuilding_source {
            if let Some(reference) = *reference {
                self.enqueue(reference);
            }
        } else {
            match self.phase {
                TracePhase::Strong => self.current_has_weak |= reference.is_some(),
                TracePhase::Ephemeron => {}
                TracePhase::EnqueueFinalization => {}
                TracePhase::ClearWeak => {
                    if let Some(target) = *reference {
                        let live = self.reference_live(target);
                        if self.error.is_none() && !live {
                            *reference = None;
                            self.stats.mark.weak_slots_cleared += 1;
                        }
                    }
                }
            }
        }
    }

    #[inline(always)]
    fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, value: &mut Value) {
        if self.rebuilding_source {
            if let Some(key) = *key {
                self.enqueue(key);
            }
            if let Some(value) = value.as_heap_ref() {
                self.enqueue(value);
            }
        } else {
            match self.phase {
                TracePhase::Strong => self.current_has_ephemeron |= key.is_some(),
                TracePhase::Ephemeron => {
                    if key.is_some_and(|key| self.reference_live(key))
                        && let Some(value) = value.as_heap_ref()
                    {
                        let before = self.stats.mark.marked_objects;
                        self.enqueue(value);
                        self.stats.mark.ephemeron_values_marked +=
                            self.stats.mark.marked_objects - before;
                    }
                }
                TracePhase::ClearWeak => {
                    let live = key.is_some_and(|key| self.reference_live(key));
                    if self.error.is_none() && !live {
                        *key = None;
                        *value = Value::from_immediate(tachyon_value::Immediate::Undefined);
                        self.stats.mark.ephemerons_cleared += 1;
                    }
                }
                TracePhase::EnqueueFinalization => {}
            }
        }
    }

    #[inline(always)]
    fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, held_value: &mut Value) {
        if self.rebuilding_source {
            if let Some(target) = *target {
                self.enqueue(target);
            }
            if let Some(held) = held_value.as_heap_ref() {
                self.enqueue(held);
            }
            return;
        }
        match self.phase {
            TracePhase::Strong => {
                self.current_has_finalization |= target.is_some();
                self.trace_value(held_value);
            }
            TracePhase::Ephemeron | TracePhase::ClearWeak => {}
            TracePhase::EnqueueFinalization => {
                let Some(target_reference) = *target else {
                    return;
                };
                let live = self.reference_live(target_reference);
                if self.error.is_some() || live {
                    return;
                }
                let registry = self
                    .current_owner
                    .expect("minor finalization callback resolves one owner");
                match self
                    .pending_finalizations
                    .try_enqueue(registry, *held_value)
                {
                    Ok(()) => {
                        *target = None;
                        *held_value = Value::from_immediate(tachyon_value::Immediate::Undefined);
                        self.stats.mark.finalizations_enqueued += 1;
                    }
                    Err(error) => self.error = Some(MarkError::FinalizationQueue(error)),
                }
            }
        }
    }
}
