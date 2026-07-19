//! ECMAScript strings represented as immutable Latin-1 or UTF-16 code-unit sequences.

use core::{cell::Cell, cmp::Ordering};
use std::hash::Hasher;
#[allow(deprecated)]
use std::hash::SipHasher;

use tachyon_gc::{GcExternalMemory, Trace, Tracer};

use crate::atom::AtomHashSeed;

/// Representation tags reserved before inline, rope, slice, and atom tuning begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum StringRepresentationTag {
    OwnedLatin1 = 0,
    OwnedUtf16 = 1,
    InlineLatin1 = 2,
    InlineUtf16 = 3,
    Rope = 4,
    Slice = 5,
    Atom = 6,
}

/// Fallible owned-backing construction without treating UTF-8 as the engine representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StringAllocationError {
    CodeUnitLengthExceeded { length: usize, maximum: usize },
    AllocationFailed,
}

#[derive(Debug)]
enum StringBacking {
    Latin1(Box<[u8]>),
    Utf16(Box<[u16]>),
}

#[derive(Clone, Copy, Debug)]
struct CachedHash {
    seed: AtomHashSeed,
    value: u64,
}

/// Immutable GC payload; its out-of-line backing is charged through `GcExternalMemory`.
#[derive(Debug)]
pub struct JsString {
    backing: StringBacking,
    cached_hash: Cell<Option<CachedHash>>,
}

/// Borrowed code-unit view used by comparisons, hashing, RegExp, and future FFI adapters.
#[derive(Clone, Copy, Debug)]
pub enum JsStringView<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u16]),
}

impl JsString {
    /// Copies exact Latin-1 bytes into immutable fallibly allocated backing.
    pub fn try_from_latin1(value: &[u8]) -> Result<Self, StringAllocationError> {
        check_code_unit_length(value.len())?;
        Ok(Self {
            backing: StringBacking::Latin1(try_boxed_copy(value)?),
            cached_hash: Cell::new(None),
        })
    }

    /// Copies UTF-16 exactly, including unpaired surrogates and an explicitly chosen 16-bit width.
    pub fn try_from_utf16(value: &[u16]) -> Result<Self, StringAllocationError> {
        check_code_unit_length(value.len())?;
        Ok(Self {
            backing: StringBacking::Utf16(try_boxed_copy(value)?),
            cached_hash: Cell::new(None),
        })
    }

    /// Takes owned code units and compresses Latin-1 results without copying wide strings.
    pub(crate) fn try_from_owned_code_units(
        value: Vec<u16>,
    ) -> Result<Self, StringAllocationError> {
        check_code_unit_length(value.len())?;
        if value.iter().all(|unit| *unit <= u16::from(u8::MAX)) {
            let mut latin1 = Vec::new();
            latin1
                .try_reserve_exact(value.len())
                .map_err(|_| StringAllocationError::AllocationFailed)?;
            latin1.extend(value.into_iter().map(|unit| unit as u8));
            return Ok(Self {
                backing: StringBacking::Latin1(latin1.into_boxed_slice()),
                cached_hash: Cell::new(None),
            });
        }
        Ok(Self {
            backing: StringBacking::Utf16(value.into_boxed_slice()),
            cached_hash: Cell::new(None),
        })
    }

    /// Encodes valid Rust Unicode into ECMAScript code units, compressing Latin-1 when possible.
    pub fn try_from_str(value: &str) -> Result<Self, StringAllocationError> {
        if value
            .chars()
            .all(|character| u32::from(character) <= u32::from(u8::MAX))
        {
            check_code_unit_length(value.chars().count())?;
            let mut latin1 = Vec::new();
            latin1
                .try_reserve_exact(value.chars().count())
                .map_err(|_| StringAllocationError::AllocationFailed)?;
            latin1.extend(value.chars().map(|character| character as u8));
            return Ok(Self {
                backing: StringBacking::Latin1(latin1.into_boxed_slice()),
                cached_hash: Cell::new(None),
            });
        }
        let code_units = value.encode_utf16().count();
        check_code_unit_length(code_units)?;
        let mut utf16 = Vec::new();
        utf16
            .try_reserve_exact(code_units)
            .map_err(|_| StringAllocationError::AllocationFailed)?;
        utf16.extend(value.encode_utf16());
        Ok(Self {
            backing: StringBacking::Utf16(utf16.into_boxed_slice()),
            cached_hash: Cell::new(None),
        })
    }

    #[must_use]
    pub const fn representation(&self) -> StringRepresentationTag {
        match self.backing {
            StringBacking::Latin1(_) => StringRepresentationTag::OwnedLatin1,
            StringBacking::Utf16(_) => StringRepresentationTag::OwnedUtf16,
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        match &self.backing {
            StringBacking::Latin1(value) => value.len(),
            StringBacking::Utf16(value) => value.len(),
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    #[inline(always)]
    pub fn code_unit_at(&self, index: usize) -> Option<u16> {
        self.as_view().code_unit_at(index)
    }

    #[must_use]
    pub const fn as_view(&self) -> JsStringView<'_> {
        match &self.backing {
            StringBacking::Latin1(value) => JsStringView::Latin1(value),
            StringBacking::Utf16(value) => JsStringView::Utf16(value),
        }
    }

    #[must_use]
    pub(crate) fn equals_latin1(&self, value: &[u8]) -> bool {
        let view = self.as_view();
        view.len() == value.len()
            && value
                .iter()
                .enumerate()
                .all(|(index, byte)| view.code_unit_at(index) == Some(u16::from(*byte)))
    }

    /// Caches the one isolate seed normally used by this string and recomputes if ownership changes.
    pub(crate) fn hash_with_seed(&self, seed: AtomHashSeed) -> u64 {
        if let Some(cached) = self.cached_hash.get()
            && cached.seed == seed
        {
            return cached.value;
        }
        let value = self.as_view().hash_with_seed(seed);
        self.cached_hash.set(Some(CachedHash { seed, value }));
        value
    }
}

impl JsStringView<'_> {
    #[must_use]
    pub const fn len(self) -> usize {
        match self {
            Self::Latin1(value) => value.len(),
            Self::Utf16(value) => value.len(),
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    #[must_use]
    #[inline(always)]
    pub fn code_unit_at(self, index: usize) -> Option<u16> {
        match self {
            Self::Latin1(value) => value.get(index).copied().map(u16::from),
            Self::Utf16(value) => value.get(index).copied(),
        }
    }

    /// Hashes identical code-unit sequences identically across the 8-bit and 16-bit representations.
    #[allow(deprecated)] // `SipHasher` is the stable keyed SipHash API; M13 may replace the backend.
    pub(crate) fn hash_with_seed(self, seed: AtomHashSeed) -> u64 {
        let mut hasher = SipHasher::new_with_keys(seed.key0(), seed.key1());
        for index in 0..self.len() {
            hasher.write(
                &self
                    .code_unit_at(index)
                    .expect("index is bounded")
                    .to_le_bytes(),
            );
        }
        hasher.finish()
    }
}

impl PartialEq for JsStringView<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && (0..self.len()).all(|index| self.code_unit_at(index) == other.code_unit_at(index))
    }
}

impl Eq for JsStringView<'_> {}

impl PartialOrd for JsStringView<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsStringView<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        for index in 0..self.len().min(other.len()) {
            let ordering = self
                .code_unit_at(index)
                .expect("index is bounded")
                .cmp(&other.code_unit_at(index).expect("index is bounded"));
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        self.len().cmp(&other.len())
    }
}

impl PartialEq for JsString {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_view() == other.as_view()
    }
}

impl Eq for JsString {}

impl Trace for JsString {
    #[inline(always)]
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for JsString {
    fn external_memory_bytes(&self) -> usize {
        match &self.backing {
            StringBacking::Latin1(value) => value.len(),
            StringBacking::Utf16(value) => value.len().saturating_mul(size_of::<u16>()),
        }
    }
}

fn check_code_unit_length(length: usize) -> Result<(), StringAllocationError> {
    if length > u32::MAX as usize {
        return Err(StringAllocationError::CodeUnitLengthExceeded {
            length,
            maximum: u32::MAX as usize,
        });
    }
    Ok(())
}

fn try_boxed_copy<T: Copy>(value: &[T]) -> Result<Box<[T]>, StringAllocationError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| StringAllocationError::AllocationFailed)?;
    owned.extend_from_slice(value);
    Ok(owned.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use tachyon_gc::{
        AllocationSpace, GcExternalMemory, Heap, HeapLimit, SPAN_SIZE_BYTES, TypeRegistry,
    };
    use tachyon_value::Value;

    use super::{JsString, JsStringView, StringRepresentationTag};
    use crate::AtomHashSeed;

    #[test]
    fn constructors_preserve_ecmascript_code_units_and_choose_explicit_widths() {
        let latin1 = JsString::try_from_str("A\u{00e9}").unwrap();
        assert_eq!(
            latin1.representation(),
            StringRepresentationTag::OwnedLatin1
        );
        assert_eq!(latin1.len(), 2);
        assert_eq!(latin1.code_unit_at(1), Some(0x00e9));

        let supplementary = JsString::try_from_str("\u{1f600}").unwrap();
        assert_eq!(
            supplementary.representation(),
            StringRepresentationTag::OwnedUtf16
        );
        assert_eq!(
            supplementary.as_view(),
            JsStringView::Utf16(&[0xd83d, 0xde00])
        );

        let unpaired = JsString::try_from_utf16(&[0xd800, b'x' as u16]).unwrap();
        assert_eq!(unpaired.code_unit_at(0), Some(0xd800));
        assert_eq!(unpaired.code_unit_at(2), None);

        let concatenated = JsString::try_from_owned_code_units(vec![b'a' as u16, 0x00e9]).unwrap();
        assert_eq!(
            concatenated.representation(),
            StringRepresentationTag::OwnedLatin1
        );
        let concatenated = JsString::try_from_owned_code_units(vec![b'a' as u16, 0xd800]).unwrap();
        assert_eq!(
            concatenated.representation(),
            StringRepresentationTag::OwnedUtf16
        );
    }

    #[test]
    fn equality_order_and_hash_use_code_units_across_backing_widths() {
        let latin1 = JsString::try_from_latin1(&[0xe9]).unwrap();
        let utf16 = JsString::try_from_utf16(&[0x00e9]).unwrap();
        let later = JsString::try_from_utf16(&[0x00ea]).unwrap();
        let seed = AtomHashSeed::new(1, 2);

        assert_eq!(latin1, utf16);
        assert_eq!(
            latin1.as_view().cmp(&later.as_view()),
            core::cmp::Ordering::Less
        );
        assert_eq!(latin1.hash_with_seed(seed), utf16.hash_with_seed(seed));
        assert_ne!(
            latin1.hash_with_seed(seed),
            latin1.hash_with_seed(AtomHashSeed::new(3, 4))
        );
    }

    #[test]
    /// String backing participates in the heap limit and is released by normal major sweep.
    fn gc_string_backing_uses_external_payload_accounting() {
        let mut types = TypeRegistry::new();
        let string_type = types.try_register::<JsString>("JsString").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES + 8), types);
        let value = JsString::try_from_utf16(&[0xd800, 1, 2, 3]).unwrap();
        assert_eq!(value.external_memory_bytes(), 8);
        let reference = heap
            .try_allocate_external(string_type, 0, value, AllocationSpace::Young)
            .unwrap();
        assert_eq!(heap.external_bytes(), 8);
        assert_eq!(
            heap.verify_reference(reference.raw(), None)
                .unwrap()
                .external_bytes(),
            Some(8)
        );

        let mut no_roots = Vec::<Value>::new();
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(heap.external_bytes(), 0);
    }
}
