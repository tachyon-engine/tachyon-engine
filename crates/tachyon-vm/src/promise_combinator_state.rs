//! Fixed managed state shared by Promise combinator iteration and element reactions.

use tachyon_gc::{GcRef, Trace, Tracer};
use tachyon_value::Value;

/// Result policy layered over the shared Promise combinator protocol driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseCombinatorKind {
    All,
    AllSettled,
    Race,
}

/// Observable operation whose completion advances a Promise combinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseCombinatorStage {
    CapabilityConstructor,
    ResolveGet,
    IteratorMethodGet,
    IteratorMethodCall,
    NextGet,
    NextCall,
    DoneGet,
    ValueGet,
    ResolveCall,
    ThenGet,
    ThenCall,
    CloseReturnGet,
    CloseReturnCall,
    CapabilityResolveCall,
    CapabilityRejectCall,
}

/// Aggregate state retained after iteration while input reactions remain pending.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingPromiseCombinator {
    pub(crate) promise: Value,
    pub(crate) values: Value,
    pub(crate) temporary: Value,
    pub(crate) capability: Value,
    pub(crate) capability_resolve: Value,
    pub(crate) capability_reject: Value,
    pub(crate) constructor: Value,
    pub(crate) promise_resolve: Value,
    pub(crate) iterable: Value,
    pub(crate) iterator: Value,
    pub(crate) next: Value,
    pub(crate) iterator_result: Value,
    pub(crate) current: Value,
    pub(crate) index: u64,
    pub(crate) remaining: u64,
    pub(crate) kind: PromiseCombinatorKind,
    pub(crate) stage: PromiseCombinatorStage,
    pub(crate) iterator_done: bool,
    pub(crate) return_promise_after_capability_call: bool,
    pub(crate) settled: bool,
}

impl Trace for PendingPromiseCombinator {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
        self.values.trace(tracer);
        self.temporary.trace(tracer);
        self.capability.trace(tracer);
        self.capability_resolve.trace(tracer);
        self.capability_reject.trace(tracer);
        self.constructor.trace(tracer);
        self.promise_resolve.trace(tracer);
        self.iterable.trace(tracer);
        self.iterator.trace(tracer);
        self.next.trace(tracer);
        self.iterator_result.trace(tracer);
        self.current.trace(tracer);
    }
}

/// Shared one-shot cell captured by both reactions for one combinator element.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseCombinatorElement {
    pub(crate) state: GcRef<PendingPromiseCombinator>,
    pub(crate) index: u64,
    pub(crate) already_called: bool,
}

impl Trace for PromiseCombinatorElement {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.state.trace(tracer);
    }
}
