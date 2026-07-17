#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Oxc-facing compilation from caller-provided source text to owned bytecode.
//!
//! Source loading remains a host responsibility; this crate intentionally has no host I/O surface.
