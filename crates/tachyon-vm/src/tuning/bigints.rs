//! BigInt allocation limits and capacity policy kept visible for profiling and embedders.

/// Bounds one materialized BigInt result to 16 Mi bits (2 MiB of canonical limbs).
///
/// Shifts and exponentiation reject larger results before allocating or entering a long loop.
/// This is an implementation resource limit, not a semantic narrowing of representable values
/// below the limit.
pub(crate) const MAX_RESULT_BITS: usize = 16 * 1024 * 1024;
