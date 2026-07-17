//! Rust-allocator-backed native storage for logical spans.

use crate::{MINIMUM_SLOT_SIZE_BYTES, SPAN_SIZE_BYTES};

const BLOCKS_PER_SPAN: usize = SPAN_SIZE_BYTES / MINIMUM_SLOT_SIZE_BYTES;

/// A structured failure to reserve one native span allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpanStorageAllocationError;

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
}

#[cfg(test)]
mod tests {
    use super::SpanStorage;
    use crate::SPAN_SIZE_BYTES;

    #[test]
    fn storage_is_exactly_one_aligned_span() {
        let storage = SpanStorage::try_new().expect("test can allocate one span");
        assert_eq!(storage.len(), SPAN_SIZE_BYTES);
        assert!(!storage.is_empty());
        assert_eq!(storage.base_address().addr() % 16, 0);
    }
}
