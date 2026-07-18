//! Number formatting knobs backed by cross-engine corpus measurements.

/// Holds the sign, radix point, and worst-case IEEE-754 exponent in both directions.
pub(crate) const RADIX_FORMAT_BUFFER_SIZE: usize = 2_200;
