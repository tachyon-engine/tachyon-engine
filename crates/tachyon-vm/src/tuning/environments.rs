//! Captured-environment cursor tuning knobs.

/// Maximum lexical-parent depth cached when one verified execution cursor is established.
///
/// Unit: environments. Most closures capture from the current or immediately enclosing function;
/// four entries cover those paths while keeping the cursor at 32 bytes. A deeper access exits to
/// the checked environment walker. Corpus tuning must compare closure throughput and cursor stack
/// pressure before changing this value.
pub(crate) const CURSOR_CACHED_DEPTH: usize = 4;

const _: () = assert!(CURSOR_CACHED_DEPTH > 0);
const _: () = assert!(CURSOR_CACHED_DEPTH <= 16);
