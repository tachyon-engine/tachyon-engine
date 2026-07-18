//! Object-model collection growth knobs awaiting M13 corpus tuning.

/// Initial shape capacity avoids reallocating for small realms without reserving the host limit.
pub(crate) const INITIAL_SHAPE_CAPACITY: usize = 32;
/// Shape storage grows in bounded chunks on the cold transition-creation path.
pub(crate) const SHAPE_GROWTH_CHUNK: usize = 64;
/// Initial transition capacity covers common literal-like construction sequences.
pub(crate) const INITIAL_TRANSITION_CAPACITY: usize = 32;
/// Transition storage grows only on misses, never on property fast-path hits.
pub(crate) const TRANSITION_GROWTH_CHUNK: usize = 64;
/// Smallest power-of-two duplicate table used by a `for-in` snapshot.
pub(crate) const MIN_FOR_IN_SEEN_CAPACITY: usize = 8;
/// A 50% maximum load keeps linear probing short without an oversized per-iterator table.
pub(crate) const FOR_IN_SEEN_LOAD_DENOMINATOR: usize = 2;
/// Multiplicative hashing disperses sequential isolate-local atom IDs before masking.
pub(crate) const FOR_IN_ATOM_HASH_MULTIPLIER: usize = 0x9E37_79B1;
