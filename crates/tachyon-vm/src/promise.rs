//! Promise state, reaction records, and the isolate-owned FIFO microtask substrate.

use std::collections::VecDeque;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseState {
    #[allow(dead_code, reason = "constructed by the Promise executor slice")]
    Pending,
    Fulfilled,
    Rejected,
}

/// One fixed-size reaction node. Linked nodes avoid reallocating a `Vec` inside a managed object.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseReaction {
    pub(crate) handler: Value,
    pub(crate) capability: Value,
    pub(crate) next: Option<GcRef<Self>>,
}

impl Trace for PromiseReaction {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.handler.trace(tracer);
        self.capability.trace(tracer);
        self.next.trace(tracer);
    }
}

/// Promise exotic payload with an ordinary property base and allocation-stable reaction lists.
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

#[allow(
    dead_code,
    reason = "consumed by the next Promise reaction execution slice"
)]
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
        }
    }
}

/// FIFO jobs are isolate-local and traced as roots until a checkpoint consumes them.
#[derive(Debug)]
pub(crate) struct PromiseJobQueue {
    jobs: VecDeque<PromiseJob>,
    active: Option<PromiseJob>,
}

impl PromiseJobQueue {
    pub(crate) fn new() -> Self {
        Self {
            jobs: VecDeque::with_capacity(tuning::promises::INITIAL_PROMISE_JOB_CAPACITY),
            active: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.jobs.len()
    }

    #[allow(
        dead_code,
        reason = "consumed by the next Promise reaction execution slice"
    )]
    pub(crate) fn push(&mut self, job: PromiseJob) {
        self.jobs.push_back(job);
    }

    /// Moves one job into a separately traced slot before any handler can allocate.
    #[allow(
        dead_code,
        reason = "consumed by the next Promise reaction execution slice"
    )]
    pub(crate) fn begin_next(&mut self) -> Option<PromiseJob> {
        debug_assert!(self.active.is_none());
        self.active = self.jobs.pop_front();
        self.active
    }

    #[allow(
        dead_code,
        reason = "consumed by the next Promise reaction execution slice"
    )]
    pub(crate) fn finish_active(&mut self) {
        self.active = None;
    }
}

impl Trace for PromiseJobQueue {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.active.trace(tracer);
        for job in &mut self.jobs {
            job.trace(tracer);
        }
    }
}

impl Isolate {
    /// Allocates one Promise with its state/result initialized before publication.
    pub(crate) fn create_promise(
        &mut self,
        state: PromiseState,
        result: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .promise_prototype
            .expect("Promise prototype initializes before Promise allocation");
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.promise_object,
                0,
                0,
                PromiseObject {
                    state,
                    result,
                    fulfill_head: None,
                    fulfill_tail: None,
                    reject_head: None,
                    reject_tail: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|promise| Value::from_heap_ref(promise.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Copies Promise state without retaining a heap borrow across an allocation.
    pub(crate) fn promise_snapshot(
        &mut self,
        value: Value,
    ) -> Result<PromiseObject, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let promise = self
            .heap
            .checked_reference(raw, self.types.promise_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let promise = scope.root(promise).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(promise, self.types.promise_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promise_jobs_move_through_the_traced_active_slot_in_fifo_order() {
        let mut queue = PromiseJobQueue::new();
        queue.push(PromiseJob::Reaction {
            handler: Value::from_i32(1),
            capability: Value::from_i32(2),
            argument: Value::from_i32(3),
            rejected: false,
        });
        queue.push(PromiseJob::Thenable {
            promise: Value::from_i32(4),
            thenable: Value::from_i32(5),
            then: Value::from_i32(6),
        });
        assert_eq!(queue.len(), 2);
        assert!(matches!(
            queue.begin_next(),
            Some(PromiseJob::Reaction { argument, .. }) if argument.as_i32() == Some(3)
        ));
        assert_eq!(queue.len(), 1);
        queue.finish_active();
        assert!(matches!(
            queue.begin_next(),
            Some(PromiseJob::Thenable { then, .. }) if then.as_i32() == Some(6)
        ));
    }
}
