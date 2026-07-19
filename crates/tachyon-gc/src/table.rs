//! Stable logical span indexing over independently allocated native storage.

use core::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use crate::{
    CollectionEpoch, GcHeader, GcRef, GcTypeId, LargeSpanMetadata, RawHeapRef, SmallObjectLayout,
    SmallObjectLayoutError, SmallSpanMetadata, SpanId, SpanReuseGeneration, SpanSpace, SpanStorage,
    SpanStorageAllocationError, Trace, TypeDescriptor,
};

mod collector;
mod publication;

pub(crate) use collector::{ReferenceSpace, SmallSweepSnapshot, SweepTarget, YoungSpanTransition};
use publication::FreeSpanRange;
pub use publication::{LargeAllocationError, LargeReclaim};

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
mod tests;
