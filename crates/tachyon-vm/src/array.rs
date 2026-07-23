//! Array exotic identity with an ordinary named-property base.

use tachyon_gc::{Trace, Tracer};

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

/// GC payload boundary reserved for packed, holey, and dictionary element storage.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ArrayObject {
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for ArrayObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::ArrayObject;
    use crate::object::OrdinaryObject;

    #[test]
    fn identity_only_array_payload_does_not_grow_the_ordinary_base() {
        assert_eq!(size_of::<ArrayObject>(), size_of::<OrdinaryObject>());
    }
}
