//! Number formatting knobs backed by cross-engine corpus measurements.

/// Holds the sign, radix point, and worst-case IEEE-754 exponent in both directions.
pub(crate) const RADIX_FORMAT_BUFFER_SIZE: usize = 2_200;

/// Holds sign, 101 significant digits, decimal point, and the largest decimal exponent.
pub(crate) const EXPONENTIAL_FORMAT_BUFFER_SIZE: usize = 112;

/// Covers the exact normalized rational for every binary64 value with room for digit extraction.
pub(crate) const DECIMAL_BIGINT_LIMBS: usize = 32;

/// Number.prototype exponential and precision methods accept at most 100 fraction digits.
pub(crate) const MAX_DECIMAL_FRACTION_DIGITS: usize = 100;
