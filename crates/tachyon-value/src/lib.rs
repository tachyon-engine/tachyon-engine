#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Bit-level JavaScript value representations and their invariants.
//!
//! This crate intentionally has no host I/O surface. Heap references are logical span addresses;
//! resolving them into pointers belongs to `tachyon-gc` with an isolate-specific span table.

use core::{
    fmt,
    num::{NonZeroU16, NonZeroU32},
};

const TAGGED_MASK: u64 = 0xfff8_0000_0000_0000;
const TAGGED_PREFIX: u64 = 0xfff8_0000_0000_0000;
const TAG_SHIFT: u32 = 48;
const TAG_MASK: u64 = 0x0007_0000_0000_0000;
const PAYLOAD_MASK: u64 = 0x0000_ffff_ffff_ffff;
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const LOW_U32_MASK: u64 = u32::MAX as u64;

const _: [(); 8] = [(); core::mem::size_of::<Value>()];
const _: [(); 4] = [(); core::mem::size_of::<RawHeapRef>()];
const _: [(); 2] = [(); core::mem::size_of::<SpanId>()];
const _: [(); 2] = [(); core::mem::size_of::<SpanOffset>()];
const _: () = assert!(TAGGED_PREFIX & !TAGGED_MASK == 0);
const _: () = assert!(TAG_MASK & TAGGED_MASK == 0);
const _: () = assert!(TAG_MASK >> TAG_SHIFT == 0b111);
const _: () = assert!(PAYLOAD_MASK & (TAGGED_MASK | TAG_MASK) == 0);

/// A stable index into one isolate's logical span table.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct SpanId(u16);

impl SpanId {
    /// Creates a span ID. Every `u16` value is part of the logical address space.
    #[must_use]
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    /// Returns the span-table index.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }
}

/// A validated non-zero byte offset within one 64 KiB logical span.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct SpanOffset(NonZeroU16);

impl SpanOffset {
    /// Creates an in-span offset, reserving zero as an invalid reference sentinel.
    #[must_use]
    pub const fn new(offset: u16) -> Option<Self> {
        match NonZeroU16::new(offset) {
            Some(offset) => Some(Self(offset)),
            None => None,
        }
    }

    /// Returns the byte offset within its logical span.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// A validated logical heap address encoded as a span ID and an in-span byte offset.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct RawHeapRef(NonZeroU32);

impl RawHeapRef {
    /// Validates an encoded logical address, rejecting every reserved zero in-span offset.
    #[must_use]
    pub const fn new(offset: u32) -> Option<Self> {
        if offset as u16 == 0 {
            return None;
        }

        match NonZeroU32::new(offset) {
            Some(offset) => Some(Self(offset)),
            None => None,
        }
    }

    /// Encodes a checked span ID and in-span offset into one 32-bit logical address.
    #[must_use]
    pub const fn from_parts(span_id: SpanId, span_offset: SpanOffset) -> Self {
        let encoded = ((span_id.index() as u32) << 16) | span_offset.get() as u32;
        match NonZeroU32::new(encoded) {
            Some(encoded) => Self(encoded),
            None => panic!("a non-zero span offset always produces a non-zero address"),
        }
    }

    /// Returns the complete logical address without resolving it against a span table.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.0.get()
    }

    /// Decodes the span-table index from this logical address.
    #[must_use]
    pub const fn span_id(self) -> SpanId {
        SpanId::new((self.offset() >> 16) as u16)
    }

    /// Decodes the validated byte offset within this logical address's span.
    #[must_use]
    pub const fn span_offset(self) -> SpanOffset {
        match SpanOffset::new(self.offset() as u16) {
            Some(offset) => offset,
            None => panic!("RawHeapRef guarantees a non-zero in-span offset"),
        }
    }
}

/// Immediate JavaScript values that do not require a heap allocation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Immediate {
    Undefined = 0,
    Null = 1,
    False = 2,
    True = 3,
    Hole = 4,
    Uninitialized = 5,
}

impl Immediate {
    const fn from_payload(payload: u64) -> Option<Self> {
        match payload {
            0 => Some(Self::Undefined),
            1 => Some(Self::Null),
            2 => Some(Self::False),
            3 => Some(Self::True),
            4 => Some(Self::Hole),
            5 => Some(Self::Uninitialized),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum Tag {
    HeapRef = 0,
    Int32 = 1,
    Immediate = 2,
}

impl Tag {
    const fn from_bits(bits: u64) -> Option<Self> {
        match ((bits & TAG_MASK) >> TAG_SHIFT) as u8 {
            0 => Some(Self::HeapRef),
            1 => Some(Self::Int32),
            2 => Some(Self::Immediate),
            _ => None,
        }
    }
}

/// The safe classification of a `Value` after validating its tag and payload invariants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueKind {
    Number(f64),
    HeapRef(RawHeapRef),
    Int32(i32),
    Immediate(Immediate),
}

/// A malformed raw bit pattern in the tagged NaN domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    InvalidHeapRefPayload(u64),
    InvalidInt32Payload(u64),
    InvalidImmediatePayload(u64),
    ReservedTag(u8),
}

/// A 64-bit NaN-boxed JavaScript value.
///
/// The tagged domain fixes the top 13 bits to `0xfff8`; bits 50..=48 hold the primary tag and
/// bits 47..=0 hold the payload. This leaves a full 48-bit payload while canonicalizing Number NaNs
/// out of the negative quiet-NaN tagged domain.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct Value(u64);

impl Value {
    /// Builds a Number value, canonicalizing every NaN to prevent it from aliasing the tagged domain.
    #[must_use]
    #[inline(always)]
    pub fn from_f64(number: f64) -> Self {
        let bits = if number.is_nan() {
            CANONICAL_NAN_BITS
        } else {
            number.to_bits()
        };
        Self(bits)
    }

    /// Builds a signed 32-bit integer value without allocating.
    #[must_use]
    #[inline(always)]
    pub const fn from_i32(value: i32) -> Self {
        Self::from_tagged(Tag::Int32, value as u32 as u64)
    }

    /// Builds a heap reference value from a validated logical span address.
    #[must_use]
    pub const fn from_heap_ref(reference: RawHeapRef) -> Self {
        Self::from_tagged(Tag::HeapRef, reference.offset() as u64)
    }

    /// Builds one of the fixed immediate values without allocating.
    #[must_use]
    pub const fn from_immediate(immediate: Immediate) -> Self {
        Self::from_tagged(Tag::Immediate, immediate as u8 as u64)
    }

    /// Preserves raw bits for verifier/fuzzing boundaries; callers must use `decode` before trusting tags.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the internal bits for engine crates; the public `tachyon` facade does not expose this API.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns the Number payload when this value is outside the tagged domain.
    #[must_use]
    #[inline(always)]
    pub const fn as_f64(self) -> Option<f64> {
        if self.is_tagged() {
            None
        } else {
            Some(f64::from_bits(self.0))
        }
    }

    /// Returns whether this value is a Number without validating any tagged payload.
    #[must_use]
    #[inline(always)]
    pub const fn is_number(self) -> bool {
        !self.is_tagged()
    }

    /// Returns the integer payload when its tag and upper payload bits are valid.
    #[must_use]
    #[inline(always)]
    pub const fn as_i32(self) -> Option<i32> {
        if self.is_tagged()
            && self.tag_bits() == Tag::Int32 as u8
            && self.payload() & !LOW_U32_MASK == 0
        {
            Some(self.payload() as u32 as i32)
        } else {
            None
        }
    }

    /// Returns a logical span address when its tag and payload are valid, without resolving it.
    #[must_use]
    pub const fn as_heap_ref(self) -> Option<RawHeapRef> {
        if !self.is_tagged()
            || self.tag_bits() != Tag::HeapRef as u8
            || self.payload() & !LOW_U32_MASK != 0
        {
            return None;
        }

        RawHeapRef::new(self.payload() as u32)
    }

    /// Returns the immediate payload when it is one of the fixed immediate encodings.
    #[must_use]
    pub const fn as_immediate(self) -> Option<Immediate> {
        if self.is_tagged() && self.tag_bits() == Tag::Immediate as u8 {
            Immediate::from_payload(self.payload())
        } else {
            None
        }
    }

    /// Validates and classifies a value without dereferencing a heap reference.
    pub fn decode(self) -> Result<ValueKind, DecodeError> {
        if let Some(number) = self.as_f64() {
            return Ok(ValueKind::Number(number));
        }

        let payload = self.0 & PAYLOAD_MASK;
        match Tag::from_bits(self.0) {
            Some(Tag::HeapRef) => Self::decode_heap_ref(payload),
            Some(Tag::Int32) => Self::decode_int32(payload),
            Some(Tag::Immediate) => Self::decode_immediate(payload),
            None => Err(DecodeError::ReservedTag(
                ((self.0 & TAG_MASK) >> TAG_SHIFT) as u8,
            )),
        }
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_tagged(self) -> bool {
        self.0 & TAGGED_MASK == TAGGED_PREFIX
    }

    #[inline(always)]
    const fn from_tagged(tag: Tag, payload: u64) -> Self {
        Self(TAGGED_PREFIX | ((tag as u64) << TAG_SHIFT) | payload)
    }

    #[inline(always)]
    const fn payload(self) -> u64 {
        self.0 & PAYLOAD_MASK
    }

    #[inline(always)]
    const fn tag_bits(self) -> u8 {
        ((self.0 & TAG_MASK) >> TAG_SHIFT) as u8
    }

    fn decode_heap_ref(payload: u64) -> Result<ValueKind, DecodeError> {
        if payload & !LOW_U32_MASK != 0 {
            return Err(DecodeError::InvalidHeapRefPayload(payload));
        }

        match RawHeapRef::new(payload as u32) {
            Some(reference) => Ok(ValueKind::HeapRef(reference)),
            None => Err(DecodeError::InvalidHeapRefPayload(payload)),
        }
    }

    fn decode_int32(payload: u64) -> Result<ValueKind, DecodeError> {
        if payload & !LOW_U32_MASK != 0 {
            return Err(DecodeError::InvalidInt32Payload(payload));
        }

        Ok(ValueKind::Int32(payload as u32 as i32))
    }

    fn decode_immediate(payload: u64) -> Result<ValueKind, DecodeError> {
        match Immediate::from_payload(payload) {
            Some(immediate) => Ok(ValueKind::Immediate(immediate)),
            None => Err(DecodeError::InvalidImmediatePayload(payload)),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Value").field(&self.0).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CANONICAL_NAN_BITS, DecodeError, Immediate, RawHeapRef, SpanId, SpanOffset, Value,
        ValueKind,
    };
    use proptest::prelude::*;

    #[test]
    fn immediate_roundtrips() {
        for immediate in [
            Immediate::Undefined,
            Immediate::Null,
            Immediate::False,
            Immediate::True,
            Immediate::Hole,
            Immediate::Uninitialized,
        ] {
            assert_eq!(
                Value::from_immediate(immediate).decode(),
                Ok(ValueKind::Immediate(immediate))
            );
        }
    }

    #[test]
    fn fast_paths_validate_tags_and_payloads() {
        let reference = RawHeapRef::new(16).expect("non-zero offset");

        assert!(Value::from_f64(1.5).is_number());
        assert_eq!(Value::from_i32(-42).as_i32(), Some(-42));
        assert_eq!(
            Value::from_heap_ref(reference).as_heap_ref(),
            Some(reference)
        );
        assert_eq!(
            Value::from_immediate(Immediate::Null).as_immediate(),
            Some(Immediate::Null)
        );
        assert_eq!(Value::from_bits(0xfff9_0001_0000_0000).as_i32(), None);
    }

    #[test]
    fn rejects_invalid_tagged_payloads() {
        assert_eq!(
            Value::from_bits(0xfff8_0000_0000_0000).decode(),
            Err(DecodeError::InvalidHeapRefPayload(0))
        );
        assert_eq!(
            Value::from_bits(0xfff8_0000_0001_0000).decode(),
            Err(DecodeError::InvalidHeapRefPayload(1 << 16))
        );
        assert_eq!(
            Value::from_bits(0xfff9_0001_0000_0000).decode(),
            Err(DecodeError::InvalidInt32Payload(0x0001_0000_0000))
        );
        assert_eq!(
            Value::from_bits(0xfffa_0000_0000_0042).decode(),
            Err(DecodeError::InvalidImmediatePayload(0x42))
        );
        assert_eq!(
            Value::from_bits(0xfffb_0000_0000_0000).decode(),
            Err(DecodeError::ReservedTag(3))
        );
    }

    #[test]
    /// Covers reserved encodings and both extremes of the 16-bit span-address partition.
    fn logical_heap_addresses_encode_and_decode_boundaries() {
        assert_eq!(SpanOffset::new(0), None);
        assert_eq!(RawHeapRef::new(0), None);
        assert_eq!(RawHeapRef::new(1_u32 << 16), None);

        let first = RawHeapRef::from_parts(
            SpanId::new(0),
            SpanOffset::new(16).expect("minimum aligned object offset"),
        );
        assert_eq!(first.offset(), 16);
        assert_eq!(first.span_id(), SpanId::new(0));
        assert_eq!(first.span_offset().get(), 16);

        let last = RawHeapRef::new(u32::MAX).expect("maximum address has a non-zero span offset");
        assert_eq!(last.span_id(), SpanId::new(u16::MAX));
        assert_eq!(last.span_offset().get(), u16::MAX);
        assert_eq!(
            RawHeapRef::from_parts(last.span_id(), last.span_offset()),
            last
        );
    }

    proptest! {
        #[test]
        fn arbitrary_f64_roundtrips_or_canonicalizes_nan(bits in any::<u64>()) {
            let input = f64::from_bits(bits);
            let output = Value::from_f64(input).as_f64().expect("numbers are never tagged");

            if input.is_nan() {
                prop_assert_eq!(output.to_bits(), CANONICAL_NAN_BITS);
            } else {
                prop_assert_eq!(output.to_bits(), bits);
            }
        }

        #[test]
        fn heap_ref_and_int32_roundtrip(
            span_id in any::<u16>(),
            span_offset in 1u16..=u16::MAX,
            integer in any::<i32>(),
        ) {
            let reference = RawHeapRef::from_parts(
                SpanId::new(span_id),
                SpanOffset::new(span_offset).expect("strategy excludes zero"),
            );
            prop_assert_eq!(Value::from_heap_ref(reference).decode(), Ok(ValueKind::HeapRef(reference)));
            prop_assert_eq!(reference.span_id(), SpanId::new(span_id));
            prop_assert_eq!(reference.span_offset().get(), span_offset);
            prop_assert_eq!(Value::from_i32(integer).decode(), Ok(ValueKind::Int32(integer)));
        }

        #[test]
        fn arbitrary_bits_decode_without_panicking(bits in any::<u64>()) {
            let _ = Value::from_bits(bits).decode();
        }
    }
}
