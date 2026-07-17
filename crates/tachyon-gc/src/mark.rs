//! Iterative strong marking over exact roots and descriptor-provided object edges.

use crate::{
    CollectionEpoch, GrayQueueError, HeapReferenceError, RawHeapRef, SpanTable, Trace, Tracer,
    TypeRegistry, gray::GrayQueue,
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
