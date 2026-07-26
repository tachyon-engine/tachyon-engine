//! Evidence-owned tuning constants for the native Signals graph.

/// Initial capacity for ordered source, sink, watched, and pending edge lists.
pub(crate) const INITIAL_EDGE_CAPACITY: usize = 4;

/// Initial capacity for the isolate-local iterative propagation worklist.
pub(crate) const INITIAL_WORKLIST_CAPACITY: usize = 16;

/// Initial capacity for one resumable multi-signal Watcher operation.
pub(crate) const INITIAL_OPERATION_CAPACITY: usize = 4;
