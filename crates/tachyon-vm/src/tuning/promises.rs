//! Promise and microtask storage guesses kept together for profiling-led adjustment.

/// Most embedded jobs settle a small fan-out without growing the isolate queue.
pub(crate) const INITIAL_PROMISE_JOB_CAPACITY: usize = 32;
