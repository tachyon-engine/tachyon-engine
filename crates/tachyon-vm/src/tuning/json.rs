//! JSON parser and serializer capacity tuning.

/// Educated initial reserve for the UTF-16 serialization buffer.
pub(crate) const INITIAL_OUTPUT_UNITS: usize = 128;

/// Educated initial reserve for the iterative container stack.
pub(crate) const INITIAL_FRAME_CAPACITY: usize = 8;

/// Doubles output storage while honoring the immediately required UTF-16 length.
pub(crate) fn grown_output_capacity(current: usize, required: usize) -> Option<usize> {
    current
        .max(INITIAL_OUTPUT_UNITS)
        .checked_mul(2)
        .map(|grown| grown.max(required))
}

/// Doubles the iterative frame stack while honoring the immediately required depth.
pub(crate) fn grown_frame_capacity(current: usize, required: usize) -> Option<usize> {
    current
        .max(INITIAL_FRAME_CAPACITY)
        .checked_mul(2)
        .map(|grown| grown.max(required))
}
