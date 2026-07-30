//! Dedicated payloads shared by the lazy Iterator Helper protocol.

use tachyon_gc::{Trace, Tracer};
use tachyon_value::Value;

use crate::object::OrdinaryObject;

/// Lazy operation selected by an `Iterator.prototype` helper method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
#[allow(
    dead_code,
    reason = "variants are constructed by the following Iterator Helper JS-surface slice"
)]
pub(crate) enum IteratorHelperKind {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
}

/// Explicit generator-like state; transitions never depend on Rust unwinding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
#[allow(
    dead_code,
    reason = "variants are transitioned by the following Iterator Helper JS-surface slice"
)]
pub(crate) enum IteratorHelperState {
    SuspendedStart,
    SuspendedYield,
    Executing,
    Completed,
}

/// GC-managed lazy helper state retained between calls to `next` and `return`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct IteratorHelperObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) outer_iterator: Value,
    pub(crate) outer_next: Value,
    /// `undefined` for helpers without a callback.
    pub(crate) callback: Value,
    /// Both inner fields are `undefined` outside an active flatMap inner iteration.
    pub(crate) inner_iterator: Value,
    pub(crate) inner_next: Value,
    pub(crate) counter_or_limit: u64,
    pub(crate) kind: IteratorHelperKind,
    pub(crate) state: IteratorHelperState,
}

const _: [(); 80] = [(); core::mem::size_of::<IteratorHelperObject>()];

impl Trace for IteratorHelperObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.outer_iterator.trace(tracer);
        self.outer_next.trace(tracer);
        self.callback.trace(tracer);
        self.inner_iterator.trace(tracer);
        self.inner_next.trace(tracer);
    }
}

/// Smaller payload used when `Iterator.from` must wrap a valid foreign iterator.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct WrapForValidIteratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) iterator: Value,
    pub(crate) next_method: Value,
}

const _: [(); 40] = [(); core::mem::size_of::<WrapForValidIteratorObject>()];

impl Trace for WrapForValidIteratorObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.iterator.trace(tracer);
        self.next_method.trace(tracer);
    }
}
