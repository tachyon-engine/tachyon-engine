//! Array exotic identity with an ordinary named-property base.

use tachyon_gc::{Trace, Tracer};

use crate::object::OrdinaryObject;

/// Largest integral Number value accepted by LengthOfArrayLike.
pub(crate) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

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
