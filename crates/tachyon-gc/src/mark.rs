//! Iterative strong marking over exact roots and descriptor-provided object edges.

use crate::{
    CardBitmap, CollectionEpoch, GrayQueueError, HeapReferenceError, RawHeapRef, SpanId, SpanTable,
    Trace, Tracer, TypeDescriptor, TypeRegistry, gray::GrayQueue, table::ReferenceSpace,
};
use tachyon_value::Value;

/// A strong-mark failure leaves partial bits in the abandoned epoch but no queued work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkError {
    InvalidReference(HeapReferenceError),
    GrayQueue(GrayQueueError),
    UnknownTypeId(RawHeapRef),
}

/// Deterministic work counters for collection tests and later pause budgeting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkStats {
    pub traced_edges: usize,
    pub marked_objects: usize,
    pub traced_objects: usize,
}

/// Deterministic young-mark and remembered-set work counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YoungMarkStats {
    pub mark: MarkStats,
    pub dirty_cards_scanned: usize,
    pub old_objects_scanned: usize,
    pub remembered_large_owners_scanned: usize,
}

/// Reaches the strong fixed point without recursively tracing through the native stack.
pub(crate) fn mark_strong_roots(
    table: &mut SpanTable,
    types: &TypeRegistry,
    gray: &mut GrayQueue,
    epoch: CollectionEpoch,
    roots: &mut dyn Trace,
) -> Result<MarkStats, MarkError> {
    gray.clear();
    let mut marker = Marker {
        table,
        gray,
        epoch,
        error: None,
        stats: MarkStats::default(),
    };
    roots.trace(&mut marker);
    marker.drain(types);
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
    epoch: CollectionEpoch,
    roots: &mut dyn Trace,
) -> Result<YoungMarkStats, MarkError> {
    gray.clear();
    let mut marker = YoungMarker {
        table,
        gray,
        epoch,
        error: None,
        stats: YoungMarkStats::default(),
        rebuilding_source: false,
        source_has_young: false,
    };
    roots.trace(&mut marker);
    marker.scan_remembered(types);
    marker.drain(types);
    if let Some(error) = marker.error {
        marker.gray.clear();
        return Err(error);
    }
    marker.rebuild_remembered(types);
    Ok(marker.stats)
}

struct Marker<'a> {
    table: &'a mut SpanTable,
    gray: &'a mut GrayQueue,
    epoch: CollectionEpoch,
    error: Option<MarkError>,
    stats: MarkStats,
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
            // SAFETY: table verification matched the live header ID to this immutable descriptor;
            // storage is independently allocated and cannot move while this marker only changes
            // side metadata and its separate gray queue. No other payload borrow is active.
            unsafe { descriptor.trace(payload, self) };
        }
    }
}

impl Tracer for Marker<'_> {
    #[inline(always)]
    fn trace_value(&mut self, value: &mut Value) {
        if let Some(reference) = value.as_heap_ref() {
            self.enqueue(reference);
        }
    }

    #[inline(always)]
    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
        self.enqueue(*reference);
    }
}

struct YoungMarker<'a> {
    table: &'a mut SpanTable,
    gray: &'a mut GrayQueue,
    epoch: CollectionEpoch,
    error: Option<MarkError>,
    stats: YoungMarkStats,
    rebuilding_source: bool,
    source_has_young: bool,
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
        // SAFETY: exact header/descriptor validation owns the same boundary as full marking. The
        // table never reallocates while tracing; only side metadata and the separate queue mutate.
        unsafe { descriptor.trace(payload, self) };
        self.rebuilding_source = false;
        self.source_has_young
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
        self.table.replace_old_cards(span_id, rebuilt);
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
        if let Some(reference) = value.as_heap_ref() {
            self.enqueue(reference);
        }
    }

    #[inline(always)]
    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
        self.enqueue(*reference);
    }
}
