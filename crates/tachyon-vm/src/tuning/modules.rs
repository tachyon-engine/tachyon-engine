//! Module-graph storage guesses awaiting M13 corpus tuning.

/// Small embedded applications commonly link a handful of source and synthetic modules.
pub(crate) const INITIAL_MODULE_CAPACITY: usize = 16;
/// Module lexical declarations and import aliases usually outnumber records by a small factor.
pub(crate) const INITIAL_BINDING_CELL_CAPACITY: usize = 32;
/// Link frames, SCC members, and export-resolution visits share this cold-path starting point.
pub(crate) const INITIAL_LINK_WORK_CAPACITY: usize = 32;
/// Most modules have one or two direct importers in embedded dependency graphs.
pub(crate) const INITIAL_ASYNC_PARENT_CAPACITY: usize = 2;
/// Completion normally releases a short chain before the next Promise turn.
pub(crate) const INITIAL_ASYNC_READY_CAPACITY: usize = 8;
/// Linear atom comparison wins for the small namespace surface common in embedded modules.
pub(crate) const NAMESPACE_LINEAR_LOOKUP_LIMIT: usize = 8;
/// Default graph edges per configured module when the host does not override module limits.
pub(crate) const DEFAULT_EDGE_CAPACITY_PER_MODULE: u32 = 16;
