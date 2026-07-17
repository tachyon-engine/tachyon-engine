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
