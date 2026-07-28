//! Managed Promise payloads, reaction nodes, and the traced FIFO job queue.

use std::collections::VecDeque;

use super::*;

pub(crate) struct PromiseCapabilityRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) promise: Value,
    pub(crate) cell: Option<GcRef<PromiseResolutionCell>>,
    pub(crate) resolve: Value,
    pub(crate) reject: Value,
}

pub(crate) struct GenericPromiseCapabilityRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) capability: Option<GcRef<PromiseCapability>>,
    pub(crate) executor: Value,
}

pub(crate) struct PromiseReactionRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) source: Value,
    pub(crate) capability: Value,
    pub(crate) handler: Value,
}

impl Trace for PromiseReactionRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.source.trace(tracer);
        self.capability.trace(tracer);
        self.handler.trace(tracer);
    }
}

impl Trace for PromiseCapabilityRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.promise.trace(tracer);
        self.cell.trace(tracer);
        self.resolve.trace(tracer);
        self.reject.trace(tracer);
    }
}

impl Trace for GenericPromiseCapabilityRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.capability.trace(tracer);
        self.executor.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

/// One fixed-size reaction node avoids reallocating a `Vec` inside a managed object.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseReaction {
    pub(crate) handler: Value,
    pub(crate) capability: Value,
    pub(crate) next: Option<GcRef<Self>>,
}

/// Shared one-shot state captured by the resolve and reject functions of one capability.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseResolutionCell {
    pub(crate) promise: Value,
    pub(crate) already_resolved: bool,
}

/// Generic NewPromiseCapability fields captured by its one native executor.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseCapability {
    pub(crate) promise: Value,
    pub(crate) resolve: Value,
    pub(crate) reject: Value,
}

impl Trace for PromiseCapability {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
        self.resolve.trace(tracer);
        self.reject.trace(tracer);
    }
}

const _: [(); 24] = [(); core::mem::size_of::<PromiseCapability>()];

impl Trace for PromiseResolutionCell {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
    }
}

impl Trace for PromiseReaction {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.handler.trace(tracer);
        self.capability.trace(tracer);
        self.next.trace(tracer);
    }
}

/// Promise exotic payload with an ordinary property base and stable reaction lists.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseObject {
    pub(crate) state: PromiseState,
    pub(crate) result: Value,
    pub(crate) fulfill_head: Option<GcRef<PromiseReaction>>,
    pub(crate) fulfill_tail: Option<GcRef<PromiseReaction>>,
    pub(crate) reject_head: Option<GcRef<PromiseReaction>>,
    pub(crate) reject_tail: Option<GcRef<PromiseReaction>>,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for PromiseObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.result.trace(tracer);
        self.fulfill_head.trace(tracer);
        self.fulfill_tail.trace(tracer);
        self.reject_head.trace(tracer);
        self.reject_tail.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PromiseJob {
    Reaction {
        handler: Value,
        capability: Value,
        argument: Value,
        rejected: bool,
    },
    Thenable {
        promise: Value,
        thenable: Value,
        then: Value,
    },
    AsyncGeneratorSettlement {
        generator: Value,
        promise: Value,
        result: Value,
        rejected: bool,
    },
}

impl Trace for PromiseJob {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        match self {
            Self::Reaction {
                handler,
                capability,
                argument,
                ..
            } => {
                handler.trace(tracer);
                capability.trace(tracer);
                argument.trace(tracer);
            }
            Self::Thenable {
                promise,
                thenable,
                then,
            } => {
                promise.trace(tracer);
                thenable.trace(tracer);
                then.trace(tracer);
            }
            Self::AsyncGeneratorSettlement {
                generator,
                promise,
                result,
                ..
            } => {
                generator.trace(tracer);
                promise.trace(tracer);
                result.trace(tracer);
            }
        }
    }
}

/// FIFO jobs remain isolate-local and rooted until a checkpoint consumes them.
#[derive(Debug)]
pub(crate) struct PromiseJobQueue {
    jobs: VecDeque<PromiseJob>,
    active: Option<PromiseJob>,
    pub(crate) checkpoint_result: Option<Value>,
}

impl PromiseJobQueue {
    pub(crate) fn new() -> Self {
        Self {
            jobs: VecDeque::with_capacity(tuning::promises::INITIAL_PROMISE_JOB_CAPACITY),
            active: None,
            checkpoint_result: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.jobs.len()
    }

    pub(crate) fn push(&mut self, job: PromiseJob) {
        self.jobs.push_back(job);
    }

    /// Moves one job into a separately traced slot before any handler can allocate.
    pub(crate) fn begin_next(&mut self) -> Option<PromiseJob> {
        debug_assert!(self.active.is_none());
        self.active = self.jobs.pop_front();
        self.active
    }

    pub(crate) fn finish_active(&mut self) {
        self.active = None;
    }

    #[inline(always)]
    pub(crate) fn has_pending(&self) -> bool {
        self.active.is_some() || !self.jobs.is_empty()
    }

    #[inline]
    pub(crate) fn begin_checkpoint(&mut self, result: Value) {
        if self.checkpoint_result.is_none() {
            self.checkpoint_result = Some(result);
        }
    }

    #[inline]
    pub(crate) fn finish_checkpoint(&mut self) -> Option<Value> {
        debug_assert!(!self.has_pending());
        self.checkpoint_result.take()
    }
}

impl Trace for PromiseJobQueue {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.active.trace(tracer);
        self.checkpoint_result.trace(tracer);
        for job in &mut self.jobs {
            job.trace(tracer);
        }
    }
}
