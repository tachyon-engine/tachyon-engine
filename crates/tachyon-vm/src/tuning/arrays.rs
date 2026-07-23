//! Array and array-like working-buffer capacity hints awaiting corpus tuning.

/// Expected primitive UTF-16 units per joined element before exact incremental reserves take over.
pub(crate) const JOIN_INITIAL_UNITS_PER_ELEMENT: usize = 8;
/// Caps speculative join reservation for sparse or adversarial array-like lengths.
pub(crate) const JOIN_MAX_INITIAL_UNITS: usize = 4_096;
/// Starts ordinary prototype-chain candidate scans instead of walking long proven hole runs.
pub(crate) const ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD: u64 = 256;
/// Covers the common small sort while avoiding length-sized allocation for sparse array-likes.
pub(crate) const INITIAL_ARRAY_SORT_ITEM_CAPACITY: usize = 64;
/// Covers ordinary shallow flattening without charging an argument-sized recursion stack.
pub(crate) const INITIAL_ARRAY_FLAT_FRAME_CAPACITY: usize = 8;

/// Doubles managed sort backing while preserving an explicit overflow boundary.
#[inline(always)]
pub(crate) const fn grown_array_sort_capacity(current: usize) -> Option<usize> {
    match current.checked_mul(2) {
        Some(capacity) if capacity > current => Some(capacity),
        _ => None,
    }
}

/// Doubles the managed flatten frame backing at an explicit overflow boundary.
#[inline(always)]
pub(crate) const fn grown_array_flat_frame_capacity(current: usize) -> Option<usize> {
    match current.checked_mul(2) {
        Some(capacity) if capacity > current => Some(capacity),
        _ => None,
    }
}
