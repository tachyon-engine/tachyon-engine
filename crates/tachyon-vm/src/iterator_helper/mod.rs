//! GC-owned substrate for synchronous Iterator Helpers.

mod filter;
mod lazy;
mod map;
mod object;

#[allow(
    unused_imports,
    reason = "helper kind and state are consumed by the following JS-surface slice"
)]
pub(crate) use object::{
    IteratorHelperKind, IteratorHelperObject, IteratorHelperState, WrapForValidIteratorObject,
};
