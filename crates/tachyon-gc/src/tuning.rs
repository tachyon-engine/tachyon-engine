//! Central GC capacity guesses that may change after benchmark evidence.

/// First table reservation: eight entries cost little while covering tiny scripts without regrowth.
pub(crate) const INITIAL_SPAN_TABLE_CAPACITY: usize = 8;
/// First free-range reservation; most heaps retain one or two coalesced ranges.
pub(crate) const INITIAL_FREE_RANGE_CAPACITY: usize = 4;
/// Numerator for bounded 1.5x metadata-container growth.
pub(crate) const CAPACITY_GROWTH_NUMERATOR: usize = 3;
/// Denominator for bounded 1.5x metadata-container growth.
pub(crate) const CAPACITY_GROWTH_DENOMINATOR: usize = 2;
