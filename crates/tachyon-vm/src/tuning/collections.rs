//! Capacity policy for insertion-ordered Map and Set backing stores.

/// First backing capacity; a corpus-driven constant, centralized for later tuning.
pub(crate) const INITIAL_ENTRY_CAPACITY: usize = 4;

/// Doubles a fixed backing when a replacement allocation is needed.
#[inline(always)]
pub(crate) const fn grown_entry_capacity(current: usize) -> Option<usize> {
    match current.checked_mul(2) {
        Some(capacity) if capacity > current => Some(capacity),
        _ => None,
    }
}
