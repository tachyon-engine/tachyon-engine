//! Resumable slow path for ordinary Math methods whose arguments require observable ToNumber.

use core::mem::size_of;

use super::*;

/// Exact argument snapshot and scalar accumulator retained across user conversion callbacks.
#[derive(Debug)]
pub(crate) struct PendingMathOperation {
    arguments: Box<[Value]>,
    cursor: usize,
    function: MathFunction,
    aggregate: f64,
    auxiliary: f64,
}

impl Trace for PendingMathOperation {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.arguments.trace(tracer);
    }
}

impl GcExternalMemory for PendingMathOperation {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.arguments.len().saturating_mul(size_of::<Value>())
    }
}

#[derive(Clone, Copy)]
struct MathOperationSnapshot {
    value: Option<Value>,
    function: MathFunction,
    aggregate: f64,
    auxiliary: f64,
}

impl Isolate {
    /// Keeps the all-primitive path allocation-free and snapshots only observable object calls.
    pub(crate) fn begin_math_operation(
        &mut self,
        function: MathFunction,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let count = math_conversion_count(function, site.argument_count);
        let mut has_object = false;
        for index in 0..count {
            let argument = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            has_object |= self.is_object_value(argument);
        }
        if !has_object {
            let result = self.math_value(function, site)?;
            return self.write(site.caller_base, site.destination, result);
        }
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(count as usize)
            .map_err(|_| ExecutionError::MathArgumentAllocationFailed)?;
        for index in 0..count {
            arguments.push(
                self.call_argument(site, index)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            );
        }
        let aggregate = if math_is_variadic(function) {
            math_variadic_initial(function)
        } else {
            0.0
        };
        let state = self.allocate_math_operation(PendingMathOperation {
            arguments: arguments.into_boxed_slice(),
            cursor: 0,
            function,
            aggregate,
            auxiliary: 0.0,
        })?;
        let continuation_site = Self::native_site(site);
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_math_operation(continuation_site, state)
    }

    /// Restores the state root before converting a callback primitive and advancing the driver.
    pub(crate) fn resume_math_argument_conversion(
        &mut self,
        site: NativeContinuationSite,
        state_value: Value,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.write(site.caller_base, site.destination, state_value)?;
        let number = self.math_primitive_number(primitive)?;
        let state_value = self.read(site.caller_base, site.destination)?;
        let state = self.pending_math_operation_reference(state_value)?;
        self.append_math_number(state, number)?;
        self.advance_math_operation(site, state)
    }

    /// Iterates all conversions without recursive Rust calls and computes only after conversion.
    fn advance_math_operation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingMathOperation>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.math_operation_snapshot(state)?;
            let Some(value) = snapshot.value else {
                let result = if math_is_variadic(snapshot.function) {
                    Value::from_f64(math_variadic_finish(
                        snapshot.function,
                        snapshot.aggregate,
                        snapshot.auxiliary,
                    ))
                } else {
                    self.finish_math_fixed(
                        snapshot.function,
                        snapshot.aggregate,
                        snapshot.auxiliary,
                    )
                };
                return self.write(site.caller_base, site.destination, result);
            };
            if self.is_object_value(value) {
                return self.dispatch_object_primitive_conversion(
                    ConversionConsumer::MathArgument,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    value,
                    site.call_site,
                );
            }
            let number = self.math_primitive_number(value)?;
            self.append_math_number(state, number)?;
        }
    }

    #[inline(always)]
    fn math_primitive_number(&mut self, value: Value) -> Result<f64, ExecutionError> {
        if self.is_bigint_value(value) {
            return Err(ExecutionError::UnsupportedNumberConversion(value));
        }
        numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))
    }

    /// Commits one converted scalar and advances the preallocated cursor atomically.
    fn append_math_number(
        &mut self,
        state: GcRef<PendingMathOperation>,
        number: f64,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_math_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if math_is_variadic(pending.function) {
                    math_variadic_add(
                        pending.function,
                        &mut pending.aggregate,
                        &mut pending.auxiliary,
                        number,
                    );
                } else if pending.cursor == 0 {
                    pending.aggregate = number;
                } else {
                    pending.auxiliary = number;
                }
                pending.cursor += 1;
                Ok(())
            })
        })
    }

    /// Copies scalar state plus the current traced argument under a no-GC borrow.
    fn math_operation_snapshot(
        &mut self,
        state: GcRef<PendingMathOperation>,
    ) -> Result<MathOperationSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_math_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(MathOperationSnapshot {
                    value: pending.arguments.get(pending.cursor).copied(),
                    function: pending.function,
                    aggregate: pending.aggregate,
                    auxiliary: pending.auxiliary,
                })
            })
        })
    }

    /// Allocates exact arguments with their external bytes included in heap accounting.
    fn allocate_math_operation(
        &mut self,
        pending: PendingMathOperation,
    ) -> Result<GcRef<PendingMathOperation>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_math_operation,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn pending_math_operation_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingMathOperation>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_math_operation)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }
}

#[inline(always)]
fn math_is_variadic(function: MathFunction) -> bool {
    matches!(
        function,
        MathFunction::Hypot | MathFunction::Max | MathFunction::Min
    )
}

#[inline(always)]
fn math_conversion_count(function: MathFunction, argument_count: u32) -> u32 {
    if matches!(function, MathFunction::Random | MathFunction::SumPrecise) {
        0
    } else if math_is_variadic(function) {
        argument_count
    } else if matches!(
        function,
        MathFunction::Atan2 | MathFunction::Imul | MathFunction::Pow
    ) {
        2
    } else {
        1
    }
}
