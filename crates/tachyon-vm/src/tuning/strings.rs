//! String and atom-table performance knobs awaiting M13 corpus tuning.

/// Initial open-addressing bucket count; power-of-two invariant is tested by `AtomTable`.
pub(crate) const INITIAL_ATOM_BUCKETS: usize = 16;
/// Maximum occupied bucket ratio numerator before rehashing.
pub(crate) const ATOM_LOAD_NUMERATOR: usize = 3;
/// Maximum occupied bucket ratio denominator before rehashing.
pub(crate) const ATOM_LOAD_DENOMINATOR: usize = 4;
