//! Full-heap diagnostic verification of non-moving generational write barriers.

use tachyon_value::{RawHeapRef, Value};

use crate::{
    HeapReferenceError, MINIMUM_SLOT_SIZE_BYTES, SpanId, SpanOffset, SpanTable, Tracer,
    TypeRegistry,
    table::{ReferenceSpace, SweepTarget},
};

/// A direct Old-to-Young edge is invalid or absent from the complete remembered-source contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BarrierVerificationError {
    InvalidSource {
        source: RawHeapRef,
        error: HeapReferenceError,
    },
    UnknownTypeId {
        source: RawHeapRef,
    },
    InvalidTarget {
        source: RawHeapRef,
        target: RawHeapRef,
        error: HeapReferenceError,
    },
    MissingSmallCard {
        source: RawHeapRef,
        target: RawHeapRef,
    },
    MissingLargeRememberedBit {
        source: RawHeapRef,
        target: RawHeapRef,
    },
    MissingRememberedSource {
        source: RawHeapRef,
        target: RawHeapRef,
    },
}

/// Exact work and edge counts from one diagnostic full-Old-heap scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BarrierVerificationStats {
    pub old_objects_scanned: usize,
    pub traced_heap_edges: usize,
    pub old_to_young_edges: usize,
    pub small_card_edges: usize,
    pub large_owner_edges: usize,
}

/// Scans every allocated Old owner without changing mark, weak, or remembered metadata.
pub(crate) fn verify_generational_barriers(
    table: &mut SpanTable,
    types: &TypeRegistry,
) -> Result<BarrierVerificationStats, BarrierVerificationError> {
    let historical_span_count = table.historical_span_count();
    let mut verifier = BarrierVerifier {
        table,
        current_source: None,
        error: None,
        stats: BarrierVerificationStats::default(),
    };
    for index in 0..historical_span_count {
        let span_index = u16::try_from(index).expect("historical span table is bounded to u16 IDs");
        let span_id = SpanId::new(span_index);
        match verifier.table.sweep_target(span_id) {
            Some(SweepTarget::Small)
                if verifier
                    .table
                    .metadata(span_id)
                    .is_some_and(|metadata| metadata.space() == crate::SpanSpace::Old) =>
            {
                verifier.scan_small_span(span_id, types);
            }
            Some(SweepTarget::LargeOwner) => verifier.scan_large_owner(span_id, types),
            Some(SweepTarget::Small) | None => {}
        }
        if let Some(error) = verifier.error {
            return Err(error);
        }
    }
    Ok(verifier.stats)
}

struct BarrierVerifier<'a> {
    table: &'a mut SpanTable,
    current_source: Option<(RawHeapRef, ReferenceSpace)>,
    error: Option<BarrierVerificationError>,
    stats: BarrierVerificationStats,
}

impl BarrierVerifier<'_> {
    /// Enumerates allocation bits instead of scanning every possible slot in an Old small span.
    fn scan_small_span(&mut self, span_id: SpanId, types: &TypeRegistry) {
        let mut start = 0;
        while let Some(source) = self.table.next_allocated_reference(span_id, start) {
            let Some(slot) = self
                .table
                .metadata(span_id)
                .and_then(|metadata| metadata.size_class().slot_for_offset(source.span_offset()))
            else {
                unreachable!("allocated references belong to the span size class");
            };
            start = slot
                .index()
                .checked_add(1)
                .expect("allocated slot index remains below u16::MAX");
            self.trace_source(source, ReferenceSpace::OldSmall, types);
            if self.error.is_some() {
                return;
            }
        }
    }

    fn scan_large_owner(&mut self, span_id: SpanId, types: &TypeRegistry) {
        let offset = SpanOffset::new(MINIMUM_SLOT_SIZE_BYTES as u16)
            .expect("large owner offset is non-zero");
        self.trace_source(
            RawHeapRef::from_parts(span_id, offset),
            ReferenceSpace::OldLarge,
            types,
        );
    }

    /// Resolves descriptor and payload at the same audited unsafe boundary used by marking.
    fn trace_source(
        &mut self,
        source: RawHeapRef,
        source_space: ReferenceSpace,
        types: &TypeRegistry,
    ) {
        let header = match self.table.verify_reference(source, None) {
            Ok(header) => header,
            Err(error) => {
                self.error = Some(BarrierVerificationError::InvalidSource { source, error });
                return;
            }
        };
        let Some(type_id) = header.type_id() else {
            self.error = Some(BarrierVerificationError::UnknownTypeId { source });
            return;
        };
        let Some(descriptor) = types.descriptor(type_id) else {
            self.error = Some(BarrierVerificationError::UnknownTypeId { source });
            return;
        };
        let payload = match self.table.payload_address(source, descriptor) {
            Ok(payload) => payload,
            Err(error) => {
                self.error = Some(BarrierVerificationError::InvalidSource { source, error });
                return;
            }
        };
        self.current_source = Some((source, source_space));
        self.stats.old_objects_scanned = self.stats.old_objects_scanned.saturating_add(1);
        // SAFETY: the live header was matched to its immutable descriptor and payload layout. The
        // verifier holds exclusive table access, retains no payload pointer after this callback,
        // and its Tracer implementation only reads side metadata without rewriting any edge.
        unsafe { descriptor.trace(payload, self) };
        self.current_source = None;
    }

    /// Checks one edge against target validity, card/owner state, and intrusive-chain membership.
    #[inline]
    fn check_edge(&mut self, target: RawHeapRef) {
        if self.error.is_some() {
            return;
        }
        let (source, source_space) = self
            .current_source
            .expect("descriptor trace runs with one current Old source");
        self.stats.traced_heap_edges = self.stats.traced_heap_edges.saturating_add(1);
        let target_space = match self.table.reference_space(target) {
            Ok(space) => space,
            Err(error) => {
                self.error = Some(BarrierVerificationError::InvalidTarget {
                    source,
                    target,
                    error,
                });
                return;
            }
        };
        if target_space != ReferenceSpace::Young {
            return;
        }
        self.stats.old_to_young_edges = self.stats.old_to_young_edges.saturating_add(1);
        match source_space {
            ReferenceSpace::OldSmall => self.check_small_edge(source, target),
            ReferenceSpace::OldLarge => self.check_large_edge(source, target),
            ReferenceSpace::Young => unreachable!("verifier only traces Old sources"),
        }
    }

    fn check_small_edge(&mut self, source: RawHeapRef, target: RawHeapRef) {
        let card_is_dirty = self
            .table
            .metadata(source.span_id())
            .is_some_and(|metadata| metadata.cards().is_dirty(source.span_offset()));
        if !card_is_dirty {
            self.error = Some(BarrierVerificationError::MissingSmallCard { source, target });
            return;
        }
        self.stats.small_card_edges = self.stats.small_card_edges.saturating_add(1);
        self.check_chain(source, target);
    }

    fn check_large_edge(&mut self, source: RawHeapRef, target: RawHeapRef) {
        let remembered = self
            .table
            .large_metadata(source.span_id())
            .is_some_and(crate::LargeSpanMetadata::is_remembered);
        if !remembered {
            self.error =
                Some(BarrierVerificationError::MissingLargeRememberedBit { source, target });
            return;
        }
        self.stats.large_owner_edges = self.stats.large_owner_edges.saturating_add(1);
        self.check_chain(source, target);
    }

    fn check_chain(&mut self, source: RawHeapRef, target: RawHeapRef) {
        if !self.table.is_in_remembered_set(source.span_id()) {
            self.error = Some(BarrierVerificationError::MissingRememberedSource { source, target });
        }
    }
}

impl Tracer for BarrierVerifier<'_> {
    #[inline(always)]
    fn trace_value(&mut self, value: &mut Value) {
        if let Some(reference) = value.as_heap_ref() {
            self.check_edge(reference);
        }
    }

    #[inline(always)]
    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
        self.check_edge(*reference);
    }

    #[inline(always)]
    fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
        if let Some(reference) = *reference {
            self.check_edge(reference);
        }
    }

    #[inline(always)]
    fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, value: &mut Value) {
        if let Some(key) = *key {
            self.check_edge(key);
        }
        self.trace_value(value);
    }

    #[inline(always)]
    fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, held_value: &mut Value) {
        if let Some(target) = *target {
            self.check_edge(target);
        }
        self.trace_value(held_value);
    }
}
