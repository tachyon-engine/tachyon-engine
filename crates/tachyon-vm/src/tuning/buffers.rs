//! Tunable fixed-memory policies for binary buffer operations.

/// Stack scratch copied per no-GC borrow pair by `ArrayBuffer.prototype.slice`.
///
/// A chunk avoids an unaccounted heap allocation while keeping the checked GC borrow
/// boundary coarse enough for large buffers. Benchmark tuning may change this value.
pub(crate) const ARRAY_BUFFER_SLICE_COPY_CHUNK_BYTES: usize = 4 * 1024;
