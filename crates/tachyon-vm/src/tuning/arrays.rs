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
/// Covers small literal/constructor arrays with one exactly charged dense backing.
pub(crate) const INITIAL_DENSE_ELEMENT_CAPACITY: usize = 8;
/// Prevents a single distant index from forcing a disproportionate contiguous allocation.
pub(crate) const MAX_DENSE_ELEMENT_INDEX: u32 = 1_048_575;
/// Sends distant writes to ordinary dictionary-style indexed properties instead of huge backing.
pub(crate) const MAX_DENSE_GROWTH_GAP: usize = 1_024;

/// Grows dense backing by 4/3 to bound allocate-copy-swap peak memory under small heap limits.
#[inline(always)]
pub(crate) const fn grown_dense_element_capacity(current: usize, required: usize) -> Option<usize> {
    let base = if current < INITIAL_DENSE_ELEMENT_CAPACITY {
        INITIAL_DENSE_ELEMENT_CAPACITY
    } else {
        current
    };
    let grown = match base.checked_add(base / 3) {
        Some(capacity) if capacity > base => capacity,
        None => return None,
        _ => return None,
    };
    Some(if grown < required { required } else { grown })
}

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

/// Grows join backing geometrically while satisfying one exact append request.
#[inline(always)]
pub(crate) const fn grown_array_join_capacity(current: usize, required: usize) -> Option<usize> {
    let base = if current == 0 { 1 } else { current };
    let doubled = match base.checked_mul(2) {
        Some(capacity) => capacity,
        None => return None,
    };
    Some(if doubled < required {
        required
    } else {
        doubled
    })
}
