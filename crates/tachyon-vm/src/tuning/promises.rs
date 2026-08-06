//! Promise and microtask storage guesses kept together for profiling-led adjustment.

/// Most embedded jobs settle a small fan-out without growing the isolate queue.
pub(crate) const INITIAL_PROMISE_JOB_CAPACITY: usize = 32;

/// Most isolates have no more than a handful of host-backed Promise operations in flight.
pub(crate) const INITIAL_ASYNC_WAIT_CAPACITY: usize = 4;

/// Bounds host registrations and persistent Promise roots independently from JS heap size.
pub(crate) const MAX_PENDING_ASYNC_WAITS: usize = 4_096;

/// Reentrant async-generator calls are rare; four requests avoid growth in common pipelines.
pub(crate) const INITIAL_ASYNC_GENERATOR_REQUEST_CAPACITY: usize = 4;
