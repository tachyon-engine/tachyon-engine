//! Fixed heap representation contracts shared by every GC phase.
//!
//! These values are layout invariants, not performance tuning knobs. Changing one changes the
//! on-heap ABI and requires a representation migration.

use core::{alloc::Layout, num::NonZeroU16};

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
/// The maximum slot count in a span after reserving the first minimum-size offset.
pub const MAX_SMALL_OBJECT_SLOTS: usize =
    (SPAN_SIZE_BYTES - MINIMUM_SLOT_SIZE_BYTES) / MINIMUM_SLOT_SIZE_BYTES;
/// Number of machine words retained per allocation or mark bitmap.
pub const SLOT_BITMAP_WORDS: usize = MAX_SMALL_OBJECT_SLOTS.div_ceil(u64::BITS as usize);
/// Remembered-set granularity for old-to-young pointer stores.
pub const CARD_SIZE_BYTES: usize = 512;
/// Number of remembered-set cards covering one span.
pub const CARDS_PER_SPAN: usize = SPAN_SIZE_BYTES / CARD_SIZE_BYTES;
/// Number of machine words retained by each per-span card bitmap.
pub const CARD_BITMAP_WORDS: usize = CARDS_PER_SPAN.div_ceil(u64::BITS as usize);
/// The byte size of every object header in the phase-1 heap.
pub const GC_HEADER_SIZE_BYTES: usize = 8;

const _: () = assert!(LOGICAL_ADDRESS_SPACE_BYTES == u32::MAX as u64 + 1);
const _: () = assert!(SPAN_SIZE_BYTES.is_power_of_two());
const _: () = assert!(MINIMUM_SLOT_SIZE_BYTES.is_power_of_two());
const _: () = assert!(SPAN_SIZE_BYTES.is_multiple_of(MINIMUM_SLOT_SIZE_BYTES));
const _: () = assert!(SPAN_SIZE_BYTES.is_multiple_of(CARD_SIZE_BYTES));
const _: () =
    assert!(MAX_LOGICAL_SPANS as u64 * SPAN_SIZE_BYTES as u64 == LOGICAL_ADDRESS_SPACE_BYTES);

/// Why a Rust payload cannot use the inline small-object representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmallObjectLayoutError {
    AlignmentTooLarge { alignment: usize },
    SizeTooLarge { size: usize },
}

/// Header-plus-payload layout before small-size-class narrowing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectLayout {
    payload_offset: usize,
    allocation_size: usize,
    alignment: usize,
}

impl ObjectLayout {
    /// Computes native header/payload placement without imposing a small-object size threshold.
    pub fn for_type<T>() -> Result<Self, SmallObjectLayoutError> {
        let payload = Layout::new::<T>();
        let (combined, payload_offset) =
            Layout::new::<GcHeader>().extend(payload).map_err(|_| {
                SmallObjectLayoutError::SizeTooLarge {
                    size: payload.size(),
                }
            })?;
        let combined = combined.pad_to_align();
        Ok(Self {
            payload_offset,
            allocation_size: combined
                .size()
                .max(MINIMUM_SLOT_SIZE_BYTES)
                .next_multiple_of(MINIMUM_SLOT_SIZE_BYTES),
            alignment: combined.align(),
        })
    }

    #[must_use]
    pub const fn payload_offset(self) -> usize {
        self.payload_offset
    }

    #[must_use]
    pub const fn allocation_size(self) -> usize {
        self.allocation_size
    }

    #[must_use]
    pub const fn alignment(self) -> usize {
        self.alignment
    }
}

/// Header-plus-payload placement within one homogeneous small-object slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmallObjectLayout {
    payload_offset: u16,
    slot_size: u16,
}

impl SmallObjectLayout {
    /// Computes the aligned payload position and rounds the complete object to a 16-byte size class.
    pub fn for_type<T>() -> Result<Self, SmallObjectLayoutError> {
        let layout = ObjectLayout::for_type::<T>()?;
        if layout.alignment() > MINIMUM_SLOT_SIZE_BYTES {
            return Err(SmallObjectLayoutError::AlignmentTooLarge {
                alignment: layout.alignment(),
            });
        }
        let payload_offset = u16::try_from(layout.payload_offset()).map_err(|_| {
            SmallObjectLayoutError::SizeTooLarge {
                size: layout.allocation_size(),
            }
        })?;
        let slot_size = u16::try_from(layout.allocation_size()).map_err(|_| {
            SmallObjectLayoutError::SizeTooLarge {
                size: layout.allocation_size(),
            }
        })?;
        Ok(Self {
            payload_offset,
            slot_size,
        })
    }

    /// Returns the byte displacement from the object header to the Rust payload.
    #[must_use]
    pub const fn payload_offset(self) -> u16 {
        self.payload_offset
    }

    /// Returns the complete 16-byte-rounded allocation size.
    #[must_use]
    pub const fn slot_size(self) -> u16 {
        self.slot_size
    }
}

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
        MINIMUM_SLOT_SIZE_BYTES, ObjectLayout, SPAN_SIZE_BYTES, SmallObjectLayout,
        SmallObjectLayoutError,
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

    #[test]
    /// Covers ordinary payload packing, padding for 16-byte payloads, and the small-space limit.
    fn small_object_layout_places_eight_and_sixteen_byte_aligned_payloads() {
        #[repr(align(16))]
        struct AlignedPayload {
            _bytes: [u8; 16],
        }
        #[repr(align(32))]
        struct OverAlignedPayload;

        let ordinary = SmallObjectLayout::for_type::<u64>().unwrap();
        assert_eq!(ordinary.payload_offset(), 8);
        assert_eq!(ordinary.slot_size(), 16);

        let aligned = SmallObjectLayout::for_type::<AlignedPayload>().unwrap();
        assert_eq!(aligned.payload_offset(), 16);
        assert_eq!(aligned.slot_size(), 32);
        assert_eq!(core::mem::size_of::<AlignedPayload>(), 16);
        assert_eq!(
            ObjectLayout::for_type::<AlignedPayload>()
                .unwrap()
                .alignment(),
            16
        );
        assert_eq!(
            SmallObjectLayout::for_type::<OverAlignedPayload>(),
            Err(SmallObjectLayoutError::AlignmentTooLarge { alignment: 32 })
        );
    }
}
