//! Shared GC-owned substrate for eager synchronous Iterator Helpers.

mod driver;
mod state;

pub(crate) use state::{IteratorEagerKind, IteratorEagerOperation};
