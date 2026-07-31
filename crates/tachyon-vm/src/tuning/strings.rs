//! String and atom-table performance knobs awaiting M13 corpus tuning.

/// Initial open-addressing bucket count; power-of-two invariant is tested by `AtomTable`.
pub(crate) const INITIAL_ATOM_BUCKETS: usize = 16;
/// Maximum occupied bucket ratio numerator before rehashing.
pub(crate) const ATOM_LOAD_NUMERATOR: usize = 3;
/// Maximum occupied bucket ratio denominator before rehashing.
pub(crate) const ATOM_LOAD_DENOMINATOR: usize = 4;
/// Expected UTF-16 units per raw template segment before measured growth takes over.
pub(crate) const RAW_INITIAL_UNITS_PER_SEGMENT: usize = 8;
/// Caps speculative String.raw reservation for adversarial array-like lengths.
pub(crate) const RAW_MAX_INITIAL_UNITS: usize = 4_096;
/// Covers common canonical/compatibility expansion without repeated UTF-16 backing growth.
pub(crate) const NORMALIZATION_INITIAL_EXPANSION_FACTOR: usize = 2;

/// Doubles String.raw backing while satisfying the current exact append request.
#[inline(always)]
pub(crate) const fn grown_raw_capacity(current: usize, required: usize) -> Option<usize> {
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
