#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Exact garbage-collection primitives independent of JavaScript object semantics.
//!
//! This crate intentionally has no host I/O surface.

mod descriptor;
mod handle;
mod layout;
mod trace;

pub use descriptor::{DropObjectFn, TraceObjectFn, TypeDescriptor};
pub use handle::GcRef;
pub use layout::{
    CAGE_SIZE_BYTES, GC_HEADER_SIZE_BYTES, GcHeader, GcTypeId, MAX_CAGE_OFFSET,
    MINIMUM_SLOT_SIZE_BYTES, SPAN_COUNT, SPAN_SIZE_BYTES,
};
pub use trace::{Trace, Tracer};
