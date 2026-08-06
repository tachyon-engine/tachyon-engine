//! Capacity guesses centralized for profile-guided adapter tuning.

/// Common DateTimeFormat output fits one medium locale pattern without growth.
pub(crate) const DATE_TIME_INITIAL_CODE_UNITS: usize = 64;

/// Typical component patterns emit fewer than sixteen fields and literals.
pub(crate) const DATE_TIME_INITIAL_PARTS: usize = 16;
