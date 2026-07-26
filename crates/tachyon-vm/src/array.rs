//! Array exotic identity with an ordinary named-property base.

use core::mem::size_of;

use tachyon_gc::{GcExternalMemory, GcRef, Trace, Tracer};
use tachyon_value::{Immediate, Value};

use crate::{object::OrdinaryObject, string::JsStringView};

/// Largest integral Number value accepted by LengthOfArrayLike.
pub(crate) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Parses one canonical safe-integer property name for ordinary sparse scans.
pub(crate) fn safe_integer_property_index(string: JsStringView<'_>) -> Option<u64> {
    let length = string.len();
    if length == 0 || length > 16 {
        return None;
    }
    let first = string.code_unit_at(0)?;
    if first == u16::from(b'0') {
        return (length == 1).then_some(0);
    }
    if !(u16::from(b'1')..=u16::from(b'9')).contains(&first) {
        return None;
    }
    let mut value = u64::from(first - u16::from(b'0'));
    for index in 1..length {
        let unit = string.code_unit_at(index)?;
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&unit) {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(unit - u16::from(b'0')))?;
        if value > MAX_SAFE_INTEGER {
            return None;
        }
    }
    Some(value)
}

/// Fixed-capacity, exactly charged backing for default Array index properties.
///
/// Published backing never changes allocation size. Capacity growth allocates a replacement and
/// switches the owning Array edge, which keeps GC external-memory accounting exact.
#[derive(Debug)]
pub(crate) struct ArrayElements {
    slots: Box<[Value]>,
    used: u32,
    hole_count: u32,
}

impl ArrayElements {
    /// Allocates a hole-filled backing without relying on implicit `Vec` capacity growth.
    pub(crate) fn with_capacity(capacity: usize) -> Result<Self, ()> {
        let mut slots = Vec::new();
        slots.try_reserve_exact(capacity).map_err(|_| ())?;
        slots.resize(capacity, Value::from_immediate(Immediate::Hole));
        Ok(Self {
            slots: slots.into_boxed_slice(),
            used: 0,
            hole_count: 0,
        })
    }

    #[inline(always)]
    pub(crate) fn value(&self, index: u32) -> Option<Value> {
        (index < self.used)
            .then(|| self.slots[index as usize])
            .filter(|value| value.as_immediate() != Some(Immediate::Hole))
    }

    #[inline(always)]
    pub(crate) fn capacity(&self) -> usize {
        self.slots.len()
    }

    #[inline(always)]
    pub(crate) const fn is_packed(&self) -> bool {
        self.hole_count == 0
    }

    #[inline(always)]
    pub(crate) const fn present_count(&self) -> u32 {
        self.used - self.hole_count
    }

    /// Stores one in-capacity index and maintains the packed/holey classification.
    pub(crate) fn set(&mut self, index: u32, value: Value) -> Result<(), ()> {
        let index = index as usize;
        if index >= self.slots.len() {
            return Err(());
        }
        let previous_used = self.used as usize;
        if index >= previous_used {
            self.hole_count = self
                .hole_count
                .checked_add(u32::try_from(index - previous_used).map_err(|_| ())?)
                .ok_or(())?;
            self.used = u32::try_from(index + 1).map_err(|_| ())?;
        } else if self.slots[index].as_immediate() == Some(Immediate::Hole) {
            self.hole_count = self.hole_count.checked_sub(1).ok_or(())?;
        }
        self.slots[index] = value;
        Ok(())
    }

    /// Marks one present index absent while retaining stable capacity.
    pub(crate) fn delete(&mut self, index: u32) -> bool {
        let Some(slot) = self.slots.get_mut(index as usize) else {
            return false;
        };
        if index >= self.used || slot.as_immediate() == Some(Immediate::Hole) {
            return false;
        }
        *slot = Value::from_immediate(Immediate::Hole);
        self.hole_count = self.hole_count.saturating_add(1);
        true
    }

    /// Clears all dense indices at or above the new Array length.
    pub(crate) fn truncate(&mut self, length: u32) {
        if length >= self.used {
            return;
        }
        self.slots[length as usize..self.used as usize]
            .fill(Value::from_immediate(Immediate::Hole));
        self.used = length;
        self.hole_count = self.slots[..length as usize]
            .iter()
            .filter(|value| value.as_immediate() == Some(Immediate::Hole))
            .count() as u32;
    }

    /// Copies live and hole slots into a larger fixed-capacity backing.
    pub(crate) fn grow_copy(&self, capacity: usize) -> Result<Self, ()> {
        if capacity < self.used as usize {
            return Err(());
        }
        let mut grown = Self::with_capacity(capacity)?;
        grown.slots[..self.used as usize].copy_from_slice(&self.slots[..self.used as usize]);
        grown.used = self.used;
        grown.hole_count = self.hole_count;
        Ok(grown)
    }

    pub(crate) fn present_indices(&self) -> impl Iterator<Item = u32> + '_ {
        debug_assert!(self.is_packed() || self.hole_count > 0);
        self.slots[..self.used as usize]
            .iter()
            .enumerate()
            .filter(|(_, value)| value.as_immediate() != Some(Immediate::Hole))
            .map(|(index, _)| index as u32)
    }
}

impl Trace for ArrayElements {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.slots.trace(tracer);
    }
}

impl GcExternalMemory for ArrayElements {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.slots.len() * size_of::<Value>()
    }
}

/// GC payload boundary reserved for packed, holey, and dictionary element storage.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ArrayObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) elements: Option<GcRef<ArrayElements>>,
}

impl Trace for ArrayObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.elements.trace(tracer);
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::{ArrayElements, ArrayObject};
    use crate::object::OrdinaryObject;
    use tachyon_value::Value;

    #[test]
    fn array_payload_adds_only_one_dense_backing_edge() {
        assert_eq!(
            size_of::<ArrayObject>(),
            size_of::<OrdinaryObject>() + size_of::<usize>()
        );
    }

    #[test]
    fn dense_backing_tracks_packed_holey_and_growth_without_capacity_mutation() {
        let mut elements = ArrayElements::with_capacity(4).expect("small test backing");
        elements.set(0, Value::from_i32(1)).expect("in bounds");
        elements.set(1, Value::from_i32(2)).expect("in bounds");
        assert!(elements.is_packed());
        assert_eq!(elements.present_count(), 2);

        assert!(elements.delete(0));
        assert!(!elements.is_packed());
        assert_eq!(elements.value(0), None);
        assert_eq!(elements.present_indices().collect::<Vec<_>>(), vec![1]);

        let grown = elements.grow_copy(8).expect("larger fixed backing");
        assert_eq!(elements.capacity(), 4);
        assert_eq!(grown.capacity(), 8);
        assert_eq!(grown.value(1), Some(Value::from_i32(2)));
    }

    #[test]
    fn dense_gap_and_truncation_keep_hole_count_exact() {
        let mut elements = ArrayElements::with_capacity(8).expect("small test backing");
        elements.set(3, Value::from_i32(4)).expect("in bounds");
        assert_eq!(elements.present_count(), 1);
        assert_eq!(elements.present_indices().collect::<Vec<_>>(), vec![3]);

        elements.set(1, Value::from_i32(2)).expect("fills one hole");
        assert_eq!(elements.present_count(), 2);
        elements.truncate(2);
        assert_eq!(elements.present_count(), 1);
        assert_eq!(elements.value(1), Some(Value::from_i32(2)));
        assert_eq!(elements.value(3), None);
    }
}
