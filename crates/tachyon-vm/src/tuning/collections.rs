//! Capacity policy for insertion-ordered Map and Set backing stores.

/// First backing capacity; a corpus-driven constant, centralized for later tuning.
pub(crate) const INITIAL_ENTRY_CAPACITY: usize = 4;

/// First avalanche multiplier for stable 32-bit logical heap addresses.
pub(crate) const WEAK_KEY_HASH_MULTIPLIER_1: u32 = 0x7FEB_352D;

/// Second avalanche multiplier for stable 32-bit logical heap addresses.
pub(crate) const WEAK_KEY_HASH_MULTIPLIER_2: u32 = 0x846C_A68B;

/// Doubles a fixed backing when a replacement allocation is needed.
#[inline(always)]
pub(crate) const fn grown_entry_capacity(current: usize) -> Option<usize> {
    match current.checked_mul(2) {
        Some(capacity) if capacity > current => Some(capacity),
        _ => None,
    }
}
