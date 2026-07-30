//! GC-owned substrate for synchronous Iterator Helpers.

mod drop;
mod filter;
mod lazy;
mod map;
mod object;
mod state;
mod take;

#[allow(
    unused_imports,
    reason = "helper kind and state are consumed by the following JS-surface slice"
)]
pub(crate) use object::{
    IteratorHelperKind, IteratorHelperObject, IteratorHelperState, WrapForValidIteratorObject,
};
