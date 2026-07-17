//! Collection epochs used to make stale mark bitmaps logically white.

use core::num::NonZeroU32;

/// A non-zero collection generation shared by one collection and its mark bitmaps.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct CollectionEpoch(NonZeroU32);

impl CollectionEpoch {
    /// The first epoch after heap creation or a coordinated overflow reset.
    pub const INITIAL: Self = Self(NonZeroU32::MIN);

    /// Creates an epoch for restoration and forced-overflow tests, rejecting zero.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the encoded non-zero generation.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    /// Advances the generation or requires the heap to reset every span bitmap on overflow.
    pub const fn next(self) -> Result<Self, CollectionEpochOverflow> {
        match self.get().checked_add(1) {
            Some(value) => match Self::new(value) {
                Some(epoch) => Ok(epoch),
                None => Err(CollectionEpochOverflow),
            },
            None => Err(CollectionEpochOverflow),
        }
    }
}

/// Signals that all span mark bitmaps must be reset before using [`CollectionEpoch::INITIAL`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollectionEpochOverflow;

#[cfg(test)]
mod tests {
    use super::{CollectionEpoch, CollectionEpochOverflow};

    #[test]
    fn epoch_zero_is_reserved_and_normal_advancement_is_monotonic() {
        assert_eq!(CollectionEpoch::new(0), None);
        assert_eq!(CollectionEpoch::INITIAL.get(), 1);
        assert_eq!(CollectionEpoch::INITIAL.next().unwrap().get(), 2);
    }

    #[test]
    fn maximum_epoch_requires_a_coordinated_bitmap_reset() {
        let maximum = CollectionEpoch::new(u32::MAX).expect("maximum is non-zero");
        assert_eq!(maximum.next(), Err(CollectionEpochOverflow));
    }
}
