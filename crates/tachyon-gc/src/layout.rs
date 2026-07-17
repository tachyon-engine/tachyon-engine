//! Fixed heap representation contracts shared by every GC phase.
//!
//! These values are layout invariants, not performance tuning knobs. Changing one changes the
//! on-heap ABI and requires a representation migration.

use core::num::NonZeroU16;

/// The representable logical heap address space; it is not a native reservation or allocation.
pub const LOGICAL_ADDRESS_SPACE_BYTES: u64 = 1_u64 << 32;
/// The largest encoded logical heap address.
pub const MAX_LOGICAL_HEAP_ADDRESS: u32 = u32::MAX;
/// The fixed allocation and side-metadata granularity for small-object spans.
pub const SPAN_SIZE_BYTES: usize = 64 * 1024;
/// The maximum number of logical span-table entries; implementations allocate them on demand.
pub const MAX_LOGICAL_SPANS: usize = 1 << 16;
/// The smallest small-object allocation slot, including its object header.
pub const MINIMUM_SLOT_SIZE_BYTES: usize = 16;
/// The byte size of every object header in the phase-1 heap.
pub const GC_HEADER_SIZE_BYTES: usize = 8;

const _: () = assert!(LOGICAL_ADDRESS_SPACE_BYTES == u32::MAX as u64 + 1);
const _: () = assert!(SPAN_SIZE_BYTES.is_power_of_two());
const _: () = assert!(MINIMUM_SLOT_SIZE_BYTES.is_power_of_two());
const _: () = assert!(SPAN_SIZE_BYTES.is_multiple_of(MINIMUM_SLOT_SIZE_BYTES));
const _: () =
    assert!(MAX_LOGICAL_SPANS as u64 * SPAN_SIZE_BYTES as u64 == LOGICAL_ADDRESS_SPACE_BYTES);

/// A non-zero index into the isolate's static GC type-descriptor table.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct GcTypeId(NonZeroU16);

impl GcTypeId {
    /// Creates a descriptor ID, reserving zero for invalid or uninitialized headers.
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the encoded descriptor-table index.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0.get()
    }
}

/// The fixed eight-byte prefix of each allocated GC object.
///
/// Mark state deliberately lives in span side metadata. Header flags may describe object lifetime
/// properties, but must never be used as the mark bitmap because epoch marking and incremental
/// collection use span side metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct GcHeader {
    type_id: u16,
    flags: u16,
    aux: u32,
}

impl GcHeader {
    /// Builds a header with the caller-defined type-specific auxiliary field.
    #[must_use]
    pub const fn new(type_id: GcTypeId, flags: u16, aux: u32) -> Self {
        Self {
            type_id: type_id.index(),
            flags,
            aux,
        }
    }

    /// Returns the static descriptor ID after validating that the header is initialized.
    #[must_use]
    pub const fn type_id(self) -> Option<GcTypeId> {
        GcTypeId::new(self.type_id)
    }

    /// Returns type-specific object flags. Marking remains exclusively side metadata.
    #[must_use]
    pub const fn flags(self) -> u16 {
        self.flags
    }

    /// Returns the type-specific compact auxiliary payload.
    #[must_use]
    pub const fn aux(self) -> u32 {
        self.aux
    }
}

const _: [(); GC_HEADER_SIZE_BYTES] = [(); core::mem::size_of::<GcHeader>()];
const _: [(); 4] = [(); core::mem::align_of::<GcHeader>()];

#[cfg(test)]
mod tests {
    use super::{
        GC_HEADER_SIZE_BYTES, GcHeader, GcTypeId, LOGICAL_ADDRESS_SPACE_BYTES, MAX_LOGICAL_SPANS,
        MINIMUM_SLOT_SIZE_BYTES, SPAN_SIZE_BYTES,
    };

    #[test]
    fn representation_constants_cover_the_logical_address_space() {
        assert_eq!(
            MAX_LOGICAL_SPANS as u64 * SPAN_SIZE_BYTES as u64,
            LOGICAL_ADDRESS_SPACE_BYTES
        );
        assert_eq!(GC_HEADER_SIZE_BYTES, core::mem::size_of::<GcHeader>());
        assert_eq!(MINIMUM_SLOT_SIZE_BYTES, 16);
    }

    #[test]
    fn header_preserves_descriptor_and_type_specific_fields() {
        let type_id = GcTypeId::new(7).expect("non-zero descriptor ID");
        let header = GcHeader::new(type_id, 0x55aa, u32::MAX);
        assert_eq!(header.type_id(), Some(type_id));
        assert_eq!(header.flags(), 0x55aa);
        assert_eq!(header.aux(), u32::MAX);
        assert_eq!(GcTypeId::new(0), None);
    }
}
