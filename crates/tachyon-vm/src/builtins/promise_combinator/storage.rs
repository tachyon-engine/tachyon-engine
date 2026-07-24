//! GC storage and rooting helpers for Promise combinators.

use super::super::super::*;

struct PendingPromiseCombinatorRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingPromiseCombinator,
}

impl Trace for PendingPromiseCombinatorRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

struct PromiseAllHandlerRoots<'a> {
    vm: VmRoots<'a>,
    state: GcRef<PendingPromiseCombinator>,
    input: Value,
    capability: Value,
    element: Option<GcRef<PromiseCombinatorElement>>,
    fulfilled: Value,
}

impl Trace for PromiseAllHandlerRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.state.trace(tracer);
        self.input.trace(tracer);
        self.capability.trace(tracer);
        self.element.trace(tracer);
        self.fulfilled.trace(tracer);
    }
}

struct PromiseAllAttachmentRoots<'a> {
    vm: VmRoots<'a>,
    state: GcRef<PendingPromiseCombinator>,
    pending: NativeCallState,
}

impl Trace for PromiseAllAttachmentRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.state.trace(tracer);
        self.pending.trace(tracer);
    }
}

struct PromiseCombinatorPrefixRoots<'a> {
    vm: VmRoots<'a>,
    state: GcRef<PendingPromiseCombinator>,
}

impl Trace for PromiseCombinatorPrefixRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.state.trace(tracer);
    }
}

impl Isolate {
    /// Creates the branded Promise.any rejection and its ordered non-enumerable errors Array.
    pub(super) fn create_promise_any_aggregate_error(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
    ) -> Result<(GcRef<PendingPromiseCombinator>, Value), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let error = self.create_native_error(NativeErrorKind::Aggregate, None)?;
        let state = self.promise_combinator_state_from_native_site(site)?;
        self.set_promise_combinator_temporary(state, error)?;
        let pending = self.promise_combinator_snapshot(state)?;
        let errors = self.intern_intrinsic_name(b"errors")?;
        self.define_data_property(
            pending.temporary,
            errors,
            DataPropertyDescriptor {
                value: Some(pending.values),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        let state = self.promise_combinator_state_from_native_site(site)?;
        let error = self.promise_combinator_snapshot(state)?.temporary;
        Ok((state, error))
    }

    /// Builds and publishes one allSettled record while refreshing roots after every allocation.
    pub(super) fn create_promise_all_settled_result(
        &mut self,
        site: &CallSite,
        state: GcRef<PendingPromiseCombinator>,
        index: u64,
        argument: Value,
        rejected: bool,
    ) -> Result<(GcRef<PendingPromiseCombinator>, Value), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.set_promise_combinator_temporary(state, argument)?;
        let index_key = self.property_key_atom(safe_integer_value(index))?;
        let status_key = self.intern_intrinsic_name(b"status")?;
        let payload_key =
            self.intern_intrinsic_name(if rejected { b"reason" } else { b"value" })?;
        let status_value_atom =
            self.intern_intrinsic_name(if rejected { b"rejected" } else { b"fulfilled" })?;

        let result = self.create_ordinary_object()?;
        let state = self.promise_combinator_state_from_site(site)?;
        let values = self.promise_combinator_snapshot(state)?.values;
        self.set_own_data_property(values, index_key, result)?;

        let status = self.atom_string_value(status_value_atom)?;
        let state = self.promise_combinator_state_from_site(site)?;
        let values = self.promise_combinator_snapshot(state)?.values;
        let result = self
            .get_data_property(values, index_key)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_own_data_property(result, status_key, status)?;

        let state = self.promise_combinator_state_from_site(site)?;
        let pending = self.promise_combinator_snapshot(state)?;
        let values = pending.values;
        let result = self
            .get_data_property(values, index_key)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_own_data_property(result, payload_key, pending.temporary)?;

        let state = self.promise_combinator_state_from_site(site)?;
        let values = self.promise_combinator_snapshot(state)?.values;
        Ok((state, values))
    }

    /// Reloads a combinator state retained in the native call destination across a safepoint.
    fn promise_combinator_state_from_site(
        &self,
        site: &CallSite,
    ) -> Result<GcRef<PendingPromiseCombinator>, ExecutionError> {
        let value = self.read(site.caller_base, site.destination)?;
        self.pending_promise_combinator_reference(value)
    }

    /// Reloads a combinator state from a continuation site that carries no argument edges.
    fn promise_combinator_state_from_native_site(
        &self,
        site: NativeContinuationSite,
    ) -> Result<GcRef<PendingPromiseCombinator>, ExecutionError> {
        let value = self.read(site.caller_base, site.destination)?;
        self.pending_promise_combinator_reference(value)
    }

    /// Reads the stable intrinsic Array length used by the first combinator fast path.
    pub(super) fn promise_all_array_length(&mut self, array: Value) -> Result<u64, ExecutionError> {
        let length_atom = self.length_atom()?;
        let length = self
            .get_data_property(array, length_atom)?
            .unwrap_or(Value::from_i32(0));
        let length = numeric_value(length).ok_or(ExecutionError::InvalidArrayLength)?;
        if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
            return Err(ExecutionError::InvalidArrayLength);
        }
        Ok(length as u64)
    }

    /// Reuses native Promises and directly fulfills non-Promise input values.
    pub(super) fn promise_all_input_promise(
        &mut self,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        if self.promise_snapshot(value).is_ok() {
            return Ok(value);
        }
        let promise = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.settle_promise(promise, PromiseState::Fulfilled, value)?;
        Ok(promise)
    }

    /// Allocates both indexed handlers while rooting the input and first allocation.
    pub(super) fn allocate_promise_all_handlers(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
        input: Value,
        capability: Value,
        index: u64,
    ) -> Result<(GcRef<PendingPromiseCombinator>, Value, Value), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function initializes before Promise.all");
        let mut roots = PromiseAllHandlerRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            state,
            input,
            capability,
            element: None,
            fulfilled: Value::from_immediate(Immediate::Undefined),
        };
        let element = self
            .heap
            .try_allocate_with_gc(
                self.types.promise_combinator_element,
                0,
                0,
                PromiseCombinatorElement {
                    state: roots.state,
                    index,
                    already_called: false,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.element = Some(element);
        let fulfilled = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::PromiseCombinatorHandler {
                        element,
                        rejected: false,
                    },
                    prototype_or_home_object: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: function_prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)?;
        roots.fulfilled = fulfilled;
        let rejected = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::PromiseCombinatorHandler {
                        element,
                        rejected: true,
                    },
                    prototype_or_home_object: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: function_prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)?;
        Ok((roots.state, roots.fulfilled, rejected))
    }

    /// Allocates one fixed aggregate record while tracing every pre-existing VM root.
    pub(super) fn allocate_pending_promise_combinator(
        &mut self,
        pending: PendingPromiseCombinator,
    ) -> Result<GcRef<PendingPromiseCombinator>, ExecutionError> {
        let mut roots = PendingPromiseCombinatorRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_promise_combinator,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates handler retention state while returning the aggregate reference relocated by GC.
    pub(super) fn allocate_promise_all_attachment(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
        pending: NativeCallState,
    ) -> Result<(GcRef<PendingPromiseCombinator>, GcRef<NativeCallState>), ExecutionError> {
        let mut roots = PromiseAllAttachmentRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            state,
            pending,
        };
        let attachment = self
            .heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok((roots.state, attachment))
    }

    /// Allocates a packed call prefix while keeping the aggregate state alive and relocatable.
    pub(super) fn allocate_promise_combinator_argument_prefix(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
        target: Value,
        this_value: Value,
        arguments: Vec<Value>,
    ) -> Result<(GcRef<PendingPromiseCombinator>, GcRef<BoundFunctionData>), ExecutionError> {
        let mut roots = PromiseCombinatorPrefixRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            state,
        };
        let prefix = self
            .heap
            .try_allocate_external_with_gc(
                self.types.bound_function,
                0,
                BoundFunctionData {
                    bound_target: target,
                    call_target: target,
                    bound_this: this_value,
                    arguments: arguments.into_boxed_slice(),
                    length: Value::from_i32(0),
                    name: Value::from_immediate(Immediate::Undefined),
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok((roots.state, prefix))
    }

    /// Copies aggregate fields without retaining a managed borrow across user-visible work.
    pub(super) fn promise_combinator_snapshot(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
    ) -> Result<PendingPromiseCombinator, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_promise_combinator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Resolves a continuation value to the dedicated aggregate record type.
    pub(crate) fn pending_promise_combinator_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingPromiseCombinator>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_promise_combinator)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Applies a scalar-only state transition without retaining a borrow across a safepoint.
    pub(super) fn update_promise_combinator(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
        update: impl FnOnce(&mut PendingPromiseCombinator),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_promise_combinator)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates one managed edge and records the old-to-young barrier at the actual owner.
    pub(super) fn set_promise_combinator_value(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
        value: Value,
        update: impl FnOnce(&mut PendingPromiseCombinator, Value),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_promise_combinator)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending, value);
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Publishes one temporary edge used to bridge the next allocation safepoint.
    pub(super) fn set_promise_combinator_temporary(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_promise_combinator)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .temporary = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Atomically consumes one element's shared once guard before any settlement work.
    pub(super) fn take_promise_combinator_element(
        &mut self,
        element: GcRef<PromiseCombinatorElement>,
    ) -> Result<Option<(GcRef<PendingPromiseCombinator>, u64)>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let element = scope.root(element).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let element = no_gc
                    .borrow_mut(element, self.types.promise_combinator_element)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if element.already_called {
                    return Ok(None);
                }
                element.already_called = true;
                Ok(Some((element.state, element.index)))
            })
        })
    }

    /// Decrements the aggregate count exactly once for a fulfilled input.
    pub(super) fn decrement_promise_combinator_remaining(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
    ) -> Result<u64, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_promise_combinator)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.remaining = pending
                    .remaining
                    .checked_sub(1)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                Ok(pending.remaining)
            })
        })
    }

    /// Accounts for one element before invoking user-provided resolve or then methods.
    pub(super) fn increment_promise_combinator_remaining(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_promise_combinator)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.remaining = pending
                    .remaining
                    .checked_add(1)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                Ok(())
            })
        })
    }

    /// Marks the aggregate terminal without allocating or changing managed edges.
    pub(super) fn set_promise_combinator_settled(
        &mut self,
        state: GcRef<PendingPromiseCombinator>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_promise_combinator)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .settled = true;
                Ok(())
            })
        })
    }
}
