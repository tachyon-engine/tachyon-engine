//! Array and array-like working-buffer capacity hints awaiting corpus tuning.

/// Expected primitive UTF-16 units per joined element before exact incremental reserves take over.
pub(crate) const JOIN_INITIAL_UNITS_PER_ELEMENT: usize = 8;
/// Caps speculative join reservation for sparse or adversarial array-like lengths.
pub(crate) const JOIN_MAX_INITIAL_UNITS: usize = 4_096;
