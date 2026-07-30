//! Allocation and no-GC accessors for the fixed-layout lazy helper payload.

use super::super::*;
use super::{IteratorHelperKind, IteratorHelperState};

struct IteratorHelperAllocationRoots<'a> {
    vm: VmRoots<'a>,
    iterator: Value,
    next_method: Value,
    callback: Value,
    prototype: Value,
}

impl Trace for IteratorHelperAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.iterator.trace(tracer);
        self.next_method.trace(tracer);
        self.callback.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Isolate {
    /// Allocates one fixed-layout lazy helper after its cached next Get succeeds.
    pub(super) fn allocate_iterator_helper(
        &mut self,
        iterator: Value,
        next_method: Value,
        callback: Value,
        kind: IteratorHelperKind,
        counter_or_limit: u64,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .iterator_helper_prototype
            .expect("Iterator Helper prototype initializes before lazy helpers");
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = IteratorHelperAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            iterator,
            next_method,
            callback,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.iterator_helper,
                0,
                0,
                IteratorHelperObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    outer_iterator: roots.iterator,
                    outer_next: roots.next_method,
                    callback: roots.callback,
                    inner_iterator: undefined,
                    inner_next: undefined,
                    counter_or_limit,
                    kind,
                    state: IteratorHelperState::SuspendedStart,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|helper| Value::from_heap_ref(helper.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Reads the branded helper payload by value before leaving a no-GC borrow.
    pub(super) fn iterator_helper_value_snapshot(
        &mut self,
        helper: Value,
    ) -> Result<IteratorHelperObject, ExecutionError> {
        let reference = self.iterator_helper_reference(helper)?;
        self.iterator_helper_snapshot(reference)
    }

    pub(super) fn iterator_helper_reference(
        &self,
        helper: Value,
    ) -> Result<GcRef<IteratorHelperObject>, ExecutionError> {
        let raw = helper
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(helper))?;
        self.heap
            .checked_reference(raw, self.types.iterator_helper)
            .map_err(|_| ExecutionError::NotObject(helper))
    }

    pub(super) fn iterator_helper_snapshot(
        &mut self,
        helper: GcRef<IteratorHelperObject>,
    ) -> Result<IteratorHelperObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let helper = scope.root(helper).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(helper, self.types.iterator_helper)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(super) fn set_iterator_helper_state(
        &mut self,
        helper: GcRef<IteratorHelperObject>,
        state: IteratorHelperState,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let helper = scope.root(helper).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(helper, self.types.iterator_helper)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.state = state;
                Ok(())
            })
        })
    }

    pub(super) fn set_iterator_helper_counter(
        &mut self,
        helper: GcRef<IteratorHelperObject>,
        counter: u64,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let helper = scope.root(helper).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(helper, self.types.iterator_helper)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.counter_or_limit = counter;
                Ok(())
            })
        })
    }

    /// Publishes or clears flatMap's cached inner iterator record with both barriers.
    pub(super) fn set_iterator_helper_inner(
        &mut self,
        helper: GcRef<IteratorHelperObject>,
        iterator: Value,
        next_method: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let helper = scope.root(helper).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(helper, self.types.iterator_helper)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.inner_iterator = iterator;
                object.inner_next = next_method;
                Ok(())
            })?;
            scope
                .write_value_barrier(helper, iterator)
                .map_err(ExecutionError::HeapReference)?;
            scope
                .write_value_barrier(helper, next_method)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    pub(super) fn complete_iterator_helper(&mut self, helper: Value) -> Result<(), ExecutionError> {
        let reference = self.iterator_helper_reference(helper)?;
        self.set_iterator_helper_state(reference, IteratorHelperState::Completed)
    }

    pub(super) fn finish_iterator_helper_done(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<(), ExecutionError> {
        let result =
            self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
        self.write(site.caller_base, site.destination, result)
    }
}
