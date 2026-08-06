//! Executor-neutral isolate-wide JavaScript job driver.

use core::{
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    task::{Context, Poll},
};

use crate::{
    ExecutionBudget, ExecutionError, Isolate, ModuleId, PromiseState, RunOutcome, Value,
    promise::PromiseCheckpointProgress,
};

/// Identifies the isolate-owned Fiber currently consuming a driver quantum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DriverActiveWork {
    Module(crate::ModuleId),
    PromiseJob,
}

/// Observable settlement of the Promise watched by a [`VmDriver`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromiseOutcome {
    Fulfilled(Value),
    Rejected(Value),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DriverProgress {
    Progressed,
    Yielded,
    Pending,
}

/// Borrows an isolate while its persistent queues and Fibers drive one Promise to settlement.
pub struct VmDriver<'a> {
    isolate: &'a mut Isolate,
    target: Value,
    module_root: Option<ModuleId>,
    quantum: NonZeroU32,
}

impl VmDriver<'_> {
    /// Returns the stable target Promise without exposing any scheduler state.
    #[must_use]
    pub const fn promise(&self) -> Value {
        self.target
    }
}

impl Future for VmDriver<'_> {
    type Output = Result<PromiseOutcome, ExecutionError>;

    /// Runs one scheduler pass while bounding every bytecode Fiber by the configured quantum.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        if let Some(outcome) = this
            .isolate
            .driver_target_outcome(this.target, this.module_root)?
        {
            return Poll::Ready(Ok(outcome));
        }
        let progress = this.isolate.advance_driver(this.quantum.get(), cx)?;
        if let Some(outcome) = this
            .isolate
            .driver_target_outcome(this.target, this.module_root)?
        {
            return Poll::Ready(Ok(outcome));
        }
        match progress {
            DriverProgress::Progressed | DriverProgress::Yielded => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            DriverProgress::Pending => Poll::Pending,
        }
    }
}

impl Isolate {
    /// Advances at most one isolate-owned scheduler transition for embedding job pumps.
    pub fn drive_jobs_once(&mut self, quantum: NonZeroU32) -> Result<bool, ExecutionError> {
        let mut context = Context::from_waker(core::task::Waker::noop());
        self.poll_jobs_once(quantum, &mut context)
    }

    /// Advances one scheduler transition while registering the embedding executor's waker.
    pub fn poll_jobs_once(
        &mut self,
        quantum: NonZeroU32,
        context: &mut Context<'_>,
    ) -> Result<bool, ExecutionError> {
        Ok(!matches!(
            self.advance_driver(quantum.get(), context)?,
            DriverProgress::Pending
        ))
    }

    /// Creates a temporary Future over isolate-owned scheduler state.
    pub fn drive_promise(
        &mut self,
        promise: Value,
        quantum: NonZeroU32,
    ) -> Result<VmDriver<'_>, ExecutionError> {
        self.promise_snapshot(promise)?;
        let module_root = self.module_graph.evaluation_root_for_promise(promise);
        Ok(VmDriver {
            isolate: self,
            target: promise,
            module_root,
            quantum,
        })
    }

    /// Reconciles module completion before observing a generic Promise target.
    fn driver_target_outcome(
        &mut self,
        target: Value,
        module_root: Option<ModuleId>,
    ) -> Result<Option<PromiseOutcome>, ExecutionError> {
        if self.promise_jobs.checkpoint_result.is_none() {
            self.settle_completed_module_promise(module_root, target)?;
        }
        let snapshot = self.promise_snapshot(target)?;
        Ok(match snapshot.state {
            PromiseState::Pending => None,
            PromiseState::Fulfilled => Some(PromiseOutcome::Fulfilled(snapshot.result)),
            PromiseState::Rejected => Some(PromiseOutcome::Rejected(snapshot.result)),
        })
    }

    /// Advances the active Fiber, a ready module, or one Promise checkpoint transition.
    fn advance_driver(
        &mut self,
        quantum: u32,
        context: &mut Context<'_>,
    ) -> Result<DriverProgress, ExecutionError> {
        if self.poll_pending_atomics_waits(context)? {
            return Ok(DriverProgress::Progressed);
        }
        let budget = ExecutionBudget {
            fuel: u64::MAX,
            quantum,
        };
        if let Some(active) = self.driver_active_work {
            let outcome = self.continue_active_work_with_budget::<{
                crate::tuning::dispatch::DEFAULT_DISPATCH_BATCH
            }>(budget)?;
            return self.finish_driver_work(active, outcome);
        }
        if self.module_graph.evaluation_start_pending() {
            self.advance_module_start_transition()?;
            return Ok(DriverProgress::Progressed);
        }
        if let Some(module) = self.module_graph.take_ready_module() {
            self.driver_active_work = Some(DriverActiveWork::Module(module));
            let outcome = self.start_ready_module_with_budget::<{
                crate::tuning::dispatch::DEFAULT_DISPATCH_BATCH
            }>(module, budget)?;
            return self.finish_driver_work(DriverActiveWork::Module(module), outcome);
        }
        if self.promise_jobs.has_pending() || self.promise_jobs.checkpoint_result.is_some() {
            let checkpoint = self.promise_checkpoint_step(
                Value::from_immediate(tachyon_value::Immediate::Undefined),
                tachyon_bytecode::WordOffset::new(0),
            )?;
            match checkpoint {
                PromiseCheckpointProgress::Suspended if !self.fiber.frames.is_empty() => {
                    self.driver_active_work = Some(DriverActiveWork::PromiseJob);
                }
                PromiseCheckpointProgress::Progressed
                | PromiseCheckpointProgress::Completed(_)
                | PromiseCheckpointProgress::Suspended => {}
            }
            return Ok(DriverProgress::Progressed);
        }
        if self.advance_dynamic_import()? {
            return Ok(DriverProgress::Progressed);
        }
        Ok(DriverProgress::Pending)
    }

    /// Persists a yielded Fiber or publishes the terminal result to its semantic owner.
    fn finish_driver_work(
        &mut self,
        active: DriverActiveWork,
        outcome: RunOutcome,
    ) -> Result<DriverProgress, ExecutionError> {
        if outcome == RunOutcome::BudgetExhausted {
            return Ok(DriverProgress::Yielded);
        }
        self.driver_active_work = None;
        match active {
            DriverActiveWork::Module(module) => {
                self.finish_ready_module_outcome(module, outcome)?;
            }
            DriverActiveWork::PromiseJob => {}
        }
        Ok(DriverProgress::Progressed)
    }
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<VmDriver<'static>>();
};
