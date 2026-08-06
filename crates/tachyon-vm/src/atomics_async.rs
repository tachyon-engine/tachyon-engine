//! Isolate-owned bridge between host async wait handles and managed Promise settlement.

use core::task::{Context, Poll};

use crate::{
    AtomicsAsyncWait, AtomicsWaitResult, ExecutionError, Isolate, JsString, NativeErrorKind,
    PersistentRootId, PromiseObject, PromiseState, Value,
};

/// One host operation paired only with an isolate-owned persistent Promise root.
pub(crate) struct PendingAtomicsWait {
    promise: PersistentRootId<PromiseObject>,
    operation: Box<dyn AtomicsAsyncWait>,
}

impl Isolate {
    /// Reserves the host registry before a provider can publish an externally visible waiter.
    pub(crate) fn reserve_pending_atomics_wait(&mut self) -> Result<(), ExecutionError> {
        if self.pending_atomics_waits.len() == crate::tuning::promises::MAX_PENDING_ASYNC_WAITS {
            return Err(ExecutionError::PropertyStorageAllocationFailed);
        }
        if self.pending_atomics_waits.len() == self.pending_atomics_waits.capacity() {
            self.pending_atomics_waits
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        }
        Ok(())
    }

    /// Creates the Promise root before any subsequent allocation can trigger collection.
    pub(crate) fn persist_atomics_wait_promise(
        &mut self,
        promise: Value,
    ) -> Result<PersistentRootId<PromiseObject>, ExecutionError> {
        let raw = promise
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(promise))?;
        let promise = self
            .heap
            .checked_reference(raw, self.types.promise_object)
            .map_err(|_| ExecutionError::NotObject(promise))?;
        self.heap.with_running_scope(|scope| {
            let local = scope.root(promise).map_err(ExecutionError::Root)?;
            scope
                .persist(local, self.types.promise_object)
                .map_err(ExecutionError::PersistentRoot)
        })
    }

    /// Publishes a provider operation after all fallible isolate-side reservation has completed.
    pub(crate) fn register_pending_atomics_wait(
        &mut self,
        persistent: PersistentRootId<PromiseObject>,
        operation: Box<dyn AtomicsAsyncWait>,
    ) {
        debug_assert!(self.pending_atomics_waits.len() < self.pending_atomics_waits.capacity());
        self.pending_atomics_waits.push(PendingAtomicsWait {
            promise: persistent,
            operation,
        });
    }

    /// Releases an immediate or failed async-wait Promise capability root.
    pub(crate) fn release_atomics_wait_promise(
        &mut self,
        persistent: PersistentRootId<PromiseObject>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            scope
                .release_persistent(persistent, self.types.promise_object)
                .map_err(ExecutionError::PersistentRoot)
        })
    }

    /// Polls host completions before ordinary microtasks so settlement jobs become runnable.
    pub(crate) fn poll_pending_atomics_waits(
        &mut self,
        context: &mut Context<'_>,
    ) -> Result<bool, ExecutionError> {
        let mut progressed = false;
        let mut index = 0;
        while index < self.pending_atomics_waits.len() {
            let outcome = self.pending_atomics_waits[index].operation.poll(context);
            let Poll::Ready(outcome) = outcome else {
                index += 1;
                continue;
            };
            let pending = self.pending_atomics_waits.swap_remove(index);
            let promise = self.resolve_pending_atomics_wait_promise(pending.promise)?;
            let settlement = (|| match outcome {
                Ok(result) => {
                    let value = self.allocate_atomics_wait_result(result)?;
                    self.settle_promise(promise, PromiseState::Fulfilled, value)
                }
                Err(_) => {
                    let reason = self.create_native_error(NativeErrorKind::Type, None)?;
                    self.settle_promise(promise, PromiseState::Rejected, reason)
                }
            })();
            let release = self.heap.with_running_scope(|scope| {
                scope
                    .release_persistent(pending.promise, self.types.promise_object)
                    .map_err(ExecutionError::PersistentRoot)
            });
            settlement?;
            release?;
            progressed = true;
        }
        Ok(progressed)
    }

    /// Reports external scheduler work without exposing provider or persistent-root details.
    #[must_use]
    pub fn has_pending_host_jobs(&self) -> bool {
        !self.pending_atomics_waits.is_empty()
    }

    /// Resolves a persistent Promise into a temporary local before copying its logical value.
    fn resolve_pending_atomics_wait_promise(
        &mut self,
        root: PersistentRootId<PromiseObject>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let local = scope
                .local_from_persistent(root, self.types.promise_object)
                .map_err(ExecutionError::PersistentResolve)?;
            Ok(Value::from_heap_ref(local.as_gc_ref().raw()))
        })
    }

    /// Materializes one canonical wait result only on the isolate thread.
    pub(crate) fn allocate_atomics_wait_result(
        &mut self,
        result: AtomicsWaitResult,
    ) -> Result<Value, ExecutionError> {
        let text = match result {
            AtomicsWaitResult::Ok => b"ok".as_slice(),
            AtomicsWaitResult::NotEqual => b"not-equal".as_slice(),
            AtomicsWaitResult::TimedOut => b"timed-out".as_slice(),
        };
        let string = JsString::try_from_latin1(text).map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(string)
    }
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<PendingAtomicsWait>();
};
