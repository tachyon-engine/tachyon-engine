//! Rust-allocator-backed native storage for logical spans.

use crate::{
    GcHeader, MINIMUM_SLOT_SIZE_BYTES, SPAN_SIZE_BYTES, SlotIndex, SmallObjectLayout,
    SmallObjectLayoutError, SpanOffset,
};

const BLOCKS_PER_SPAN: usize = SPAN_SIZE_BYTES / MINIMUM_SLOT_SIZE_BYTES;

/// A structured failure to reserve one native span allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpanStorageAllocationError;

/// A rejected typed access before any native pointer is dereferenced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanStorageAccessError {
    MisalignedObjectOffset(SpanOffset),
    ObjectCrossesSpanEnd(SpanOffset),
    InvalidPayloadLayout(SmallObjectLayoutError),
}

#[derive(Clone, Copy)]
#[repr(C, align(16))]
struct AlignedBlock([u8; MINIMUM_SLOT_SIZE_BYTES]);

const _: () = assert!(core::mem::align_of::<AlignedBlock>() == MINIMUM_SLOT_SIZE_BYTES);
const _: () = assert!(core::mem::size_of::<AlignedBlock>() == MINIMUM_SLOT_SIZE_BYTES);

/// Stable 16-byte-aligned 64 KiB storage obtained only through Rust's global allocator.
pub struct SpanStorage {
    blocks: Vec<AlignedBlock>,
}

impl SpanStorage {
    /// Allocates and initializes one complete span without an infallible growth operation.
    pub fn try_new() -> Result<Self, SpanStorageAllocationError> {
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(BLOCKS_PER_SPAN)
            .map_err(|_| SpanStorageAllocationError)?;
        blocks.resize(BLOCKS_PER_SPAN, AlignedBlock([0; MINIMUM_SLOT_SIZE_BYTES]));
        Ok(Self { blocks })
    }

    /// Returns the stable native base address; object access must still validate side metadata.
    #[must_use]
    pub fn base_address(&self) -> *const u8 {
        self.blocks.as_ptr().cast()
    }

    /// Returns the fixed initialized storage length.
    #[must_use]
    pub const fn len(&self) -> usize {
        SPAN_SIZE_BYTES
    }

    /// Returns false because every valid storage allocation is exactly one logical span.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Initializes a complete header and payload after validating alignment and the span tail.
    pub(crate) fn initialize<T>(
        &mut self,
        offset: SpanOffset,
        header: GcHeader,
        value: T,
    ) -> Result<(), SpanStorageAccessError> {
        let layout = SmallObjectLayout::for_type::<T>()
            .map_err(SpanStorageAccessError::InvalidPayloadLayout)?;
        let object_offset = offset.get() as usize;
        if !object_offset.is_multiple_of(MINIMUM_SLOT_SIZE_BYTES) {
            return Err(SpanStorageAccessError::MisalignedObjectOffset(offset));
        }
        if object_offset + layout.slot_size() as usize > SPAN_SIZE_BYTES {
            return Err(SpanStorageAccessError::ObjectCrossesSpanEnd(offset));
        }

        let object = self
            .blocks
            .as_mut_ptr()
            .cast::<u8>()
            .wrapping_add(object_offset);
        let payload = object
            .wrapping_add(layout.payload_offset() as usize)
            .cast::<T>();
        debug_assert_eq!(object.addr() % core::mem::align_of::<GcHeader>(), 0);
        debug_assert_eq!(payload.addr() % core::mem::align_of::<T>(), 0);
        // SAFETY: the checks above keep both writes inside uniquely borrowed aligned span storage;
        // the allocator calls this only for a slot absent from the allocation bitmap.
        unsafe {
            object.cast::<GcHeader>().write(header);
            payload.write(value);
        }
        Ok(())
    }

    /// Reads a copy header only after the table verifier has established a small-object boundary.
    pub(crate) fn header(&self, offset: SpanOffset) -> Result<GcHeader, SpanStorageAccessError> {
        let object_offset = offset.get() as usize;
        if !object_offset.is_multiple_of(MINIMUM_SLOT_SIZE_BYTES) {
            return Err(SpanStorageAccessError::MisalignedObjectOffset(offset));
        }
        if object_offset + core::mem::size_of::<GcHeader>() > SPAN_SIZE_BYTES {
            return Err(SpanStorageAccessError::ObjectCrossesSpanEnd(offset));
        }
        let object = self
            .blocks
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(object_offset);
        // SAFETY: the checked offset is in bounds and 16-byte alignment satisfies `GcHeader`.
        Ok(unsafe { object.cast::<GcHeader>().read() })
    }

    /// Stores a free-list link in dead slot bytes without typed pointer access.
    pub(crate) fn write_free_next(&mut self, offset: SpanOffset, next: Option<SlotIndex>) {
        let block = &mut self.blocks[offset.get() as usize / MINIMUM_SLOT_SIZE_BYTES];
        let encoded = next.map_or(0, |slot| slot.index() + 1).to_ne_bytes();
        block.0[..encoded.len()].copy_from_slice(&encoded);
    }

    /// Decodes a previously stored free-list link; zero is the end sentinel, not slot zero.
    pub(crate) fn read_free_next(&self, offset: SpanOffset) -> Option<SlotIndex> {
        let block = &self.blocks[offset.get() as usize / MINIMUM_SLOT_SIZE_BYTES];
        let encoded = u16::from_ne_bytes([block.0[0], block.0[1]]);
        encoded.checked_sub(1).and_then(SlotIndex::new)
    }
}

#[cfg(test)]
mod tests {
    use super::{SpanStorage, SpanStorageAccessError};
    use crate::{GcHeader, GcTypeId, SPAN_SIZE_BYTES, SlotIndex, SpanOffset};

    #[test]
    fn storage_is_exactly_one_aligned_span() {
        let storage = SpanStorage::try_new().expect("test can allocate one span");
        assert_eq!(storage.len(), SPAN_SIZE_BYTES);
        assert!(!storage.is_empty());
        assert_eq!(storage.base_address().addr() % 16, 0);
    }

    #[test]
    /// Exercises the local unsafe write/read boundary at ordinary, over-aligned, and tail offsets.
    fn typed_initialization_checks_alignment_and_span_tail() {
        #[derive(Clone, Copy)]
        #[repr(align(16))]
        struct AlignedPayload {
            _bytes: [u8; 16],
        }

        let mut storage = SpanStorage::try_new().unwrap();
        let header = GcHeader::new(GcTypeId::new(3).unwrap(), 7, 11);
        let first = SpanOffset::new(16).unwrap();
        storage
            .initialize(first, header, AlignedPayload { _bytes: [0xaa; 16] })
            .unwrap();
        assert_eq!(storage.header(first).unwrap(), header);

        let misaligned = SpanOffset::new(17).unwrap();
        assert_eq!(
            storage.initialize(misaligned, header, 1_u64),
            Err(SpanStorageAccessError::MisalignedObjectOffset(misaligned))
        );
        let tail = SpanOffset::new(65_520).unwrap();
        assert_eq!(
            storage.initialize(tail, header, AlignedPayload { _bytes: [0; 16] }),
            Err(SpanStorageAccessError::ObjectCrossesSpanEnd(tail))
        );
    }

    #[test]
    fn free_list_links_round_trip_slot_zero_and_end_sentinel() {
        let mut storage = SpanStorage::try_new().unwrap();
        let offset = SpanOffset::new(16).unwrap();
        let slot_zero = SlotIndex::new(0).unwrap();
        storage.write_free_next(offset, Some(slot_zero));
        assert_eq!(storage.read_free_next(offset), Some(slot_zero));
        storage.write_free_next(offset, None);
        assert_eq!(storage.read_free_next(offset), None);
    }
}
