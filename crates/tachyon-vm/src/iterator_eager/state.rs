//! Fixed-layout state retained across eager Iterator Helper callbacks.

use super::super::*;

/// Eager operation selected by an `Iterator.prototype` method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IteratorEagerKind {
    Reduce,
    ToArray,
    ForEach,
    Some,
    Every,
    Find,
    SumPrecise,
}

/// Non-object GC payload that owns all values live across observable boundaries.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct IteratorEagerOperation {
    pub(crate) iterator: Value,
    pub(crate) next_method: Value,
    pub(crate) callback: Value,
    pub(crate) accumulator_or_output: Value,
    pub(crate) current_value: Value,
    pub(crate) counter: u64,
    pub(crate) kind: IteratorEagerKind,
    pub(crate) has_accumulator: bool,
}

impl Trace for IteratorEagerOperation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.iterator.trace(tracer);
        self.next_method.trace(tracer);
        self.callback.trace(tracer);
        self.accumulator_or_output.trace(tracer);
        self.current_value.trace(tracer);
    }
}

struct IteratorEagerAllocationRoots<'a> {
    vm: VmRoots<'a>,
    operation: IteratorEagerOperation,
}

impl Trace for IteratorEagerAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.operation.trace(tracer);
    }
}

impl Isolate {
    /// Allocates one eager operation with every stage-crossing Value already traced.
    pub(super) fn allocate_iterator_eager_operation(
        &mut self,
        operation: IteratorEagerOperation,
    ) -> Result<GcRef<IteratorEagerOperation>, ExecutionError> {
        let mut roots = IteratorEagerAllocationRoots {
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
            operation,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.iterator_eager_operation,
                0,
                0,
                roots.operation,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(super) fn iterator_eager_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IteratorEagerOperation>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.iterator_eager_operation)
            .map_err(ExecutionError::HeapReference)
    }

    pub(super) fn iterator_eager_snapshot(
        &mut self,
        state: GcRef<IteratorEagerOperation>,
    ) -> Result<IteratorEagerOperation, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.iterator_eager_operation)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Mutates scalar state and publishes changed Value fields with precise barriers.
    pub(super) fn update_iterator_eager(
        &mut self,
        state: GcRef<IteratorEagerOperation>,
        update: impl FnOnce(&mut IteratorEagerOperation),
    ) -> Result<(), ExecutionError> {
        let values = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            let values = scope.with_no_gc_scope(|no_gc| {
                let operation = no_gc
                    .borrow_mut(state, self.types.iterator_eager_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(operation);
                Ok::<_, ExecutionError>([
                    operation.next_method,
                    operation.accumulator_or_output,
                    operation.current_value,
                ])
            })?;
            for value in values {
                scope
                    .write_value_barrier(state, value)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok::<_, ExecutionError>(values)
        })?;
        let _ = values;
        Ok(())
    }
}
