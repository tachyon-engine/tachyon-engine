//! Module-graph storage guesses awaiting M13 corpus tuning.

/// Small embedded applications commonly link a handful of source and synthetic modules.
pub(crate) const INITIAL_MODULE_CAPACITY: usize = 16;
/// Module lexical declarations and import aliases usually outnumber records by a small factor.
pub(crate) const INITIAL_BINDING_CELL_CAPACITY: usize = 32;
/// Link frames, SCC members, and export-resolution visits share this cold-path starting point.
pub(crate) const INITIAL_LINK_WORK_CAPACITY: usize = 32;
