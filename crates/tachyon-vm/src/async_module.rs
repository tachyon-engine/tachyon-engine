//! Module-owned top-level Await Fiber transfer state.

use super::*;

/// A TLA body owns its paused Fiber independently from async-function completion semantics.
pub(crate) struct AsyncModuleState {
    module: ModuleId,
    caller: Option<Fiber>,
    paused: Option<Fiber>,
    await_destination: Option<u32>,
    await_instruction: Option<WordOffset>,
}

impl Trace for AsyncModuleState {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        if let Some(caller) = &mut self.caller {
            caller.trace_roots(tracer);
        }
        if let Some(paused) = &mut self.paused {
            paused.trace_roots(tracer);
        }
    }
}

impl Isolate {
    /// Starts one TLA body with a bounded interpreter budget.
    pub(crate) fn begin_async_module_with_budget<const N: usize>(
        &mut self,
        code: CodeId,
        module: ModuleId,
        budget: ExecutionBudget,
    ) -> Result<(Value, RunOutcome), ExecutionError> {
        let mut roots = VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        let state = self
            .heap
            .try_allocate_with_gc(
                self.types.async_module_state,
                0,
                0,
                AsyncModuleState {
                    module,
                    caller: Some(Fiber::default()),
                    paused: None,
                    await_destination: None,
                    await_instruction: None,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let state_value = Value::from_heap_ref(state.raw());
        self.module_graph
            .begin_async_evaluation(module, state_value)
            .map_err(ExecutionError::Module)?;
        if let Err(error) = self.enter_with_parent(code, FunctionId::new(0), None, Some(module)) {
            self.module_graph
                .reset_async_evaluation(module)
                .map_err(ExecutionError::Module)?;
            return Err(error);
        }
        if let Err(error) = self
            .fiber
            .completions
            .push_native(NativeContinuation::async_module(
                NativeContinuationSite {
                    caller_base: 0,
                    destination: 0,
                    call_site: WordOffset::new(0),
                },
                state_value,
            ))
        {
            self.fiber = Fiber::default();
            self.module_graph
                .reset_async_evaluation(module)
                .map_err(ExecutionError::Module)?;
            return Err(Self::completion_stack_error(error));
        }
        let frame = self
            .fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?;
        frame.return_continuation = true;
        frame.completion_base = 1;
        let outcome = match self.continue_active_work_with_budget::<N>(budget) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.fiber = Fiber::default();
                self.module_graph
                    .reset_async_evaluation(module)
                    .map_err(ExecutionError::Module)?;
                return Err(error);
            }
        };
        Ok((state_value, outcome))
    }

    /// Verifies the suspend point and routes PromiseResolve through the module owner.
    pub(crate) fn suspend_async_module_await(
        &mut self,
        site: crate::async_function::AsyncAwaitSite,
    ) -> Result<(), ExecutionError> {
        let frame = self
            .fiber
            .frames
            .last()
            .copied()
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        let continuation = frame
            .completion_base
            .checked_sub(1)
            .and_then(|index| self.fiber.completions.native_at(index as usize))
            .filter(|continuation| continuation.kind() == NativeContinuationKind::AsyncModule)
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        let state_value = continuation.first();
        let state = self.async_module_state_reference(state_value)?;
        let point = self
            .loaded_code(site.code)?
            .module
            .function(frame.function)
            .and_then(|function| {
                function
                    .suspend_points()
                    .get(site.suspend_id as usize)
                    .copied()
            })
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        if point.instruction != site.instruction
            || point.destination.index() != site.destination
            || point.resume_offset != frame.pc
        {
            return Err(ExecutionError::UnsupportedAsyncFunctionResume);
        }
        self.prepare_async_module_await(state, site.destination, site.instruction)?;
        let source = self.read(site.base, site.source)?;
        if self.promise_snapshot(source).is_ok() {
            return self.begin_async_await_constructor_get(
                NativeContinuationSite {
                    caller_base: site.base,
                    destination: site.destination,
                    call_site: site.instruction,
                },
                state_value,
                source,
            );
        }
        let awaited = self.create_promise(
            if self.is_object_value(source) {
                PromiseState::Pending
            } else {
                PromiseState::Fulfilled
            },
            if self.is_object_value(source) {
                Value::from_immediate(Immediate::Undefined)
            } else {
                source
            },
        )?;
        self.perform_promise_then_with_capability(awaited, None, None, state_value)?;
        if self.is_object_value(source) {
            self.begin_promise_resolution(
                awaited,
                source,
                NativeContinuationSite {
                    caller_base: site.base,
                    destination: site.destination,
                    call_site: site.instruction,
                },
                PromiseResolutionMode::AsyncAwait,
            )
        } else {
            self.complete_async_await_resolution()
        }
    }

    /// Swaps the checkpoint driver for the paused module body and resumes Normal or Throw.
    pub(crate) fn resume_async_module_job(
        &mut self,
        state_value: Value,
        argument: Value,
        rejected: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.async_module_state_reference(state_value)?;
        let caller = core::mem::take(&mut self.fiber);
        let (paused, destination, instruction) = self.take_async_module_pause(state, caller)?;
        self.fiber = paused;
        self.promise_jobs.finish_active();
        let base = self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::MissingEnvironment)?
            .base;
        if rejected {
            self.dispatch_abrupt(CompletionRecord::throw(argument), instruction)
        } else {
            self.write(base, destination, argument)?;
            Ok(None)
        }
    }

    pub(crate) fn finish_async_module_return(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.async_module_state_reference(continuation.first())?;
        let module = self.async_module_id(state)?;
        self.module_graph
            .finish_async_evaluation(module, Ok(value))?;
        self.restore_async_module_caller(state)
    }

    pub(crate) fn finish_async_module_throw(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.async_module_state_reference(continuation.first())?;
        let module = self.async_module_id(state)?;
        self.module_graph
            .finish_async_evaluation(module, Err(reason))?;
        self.restore_async_module_caller(state)
    }

    #[inline]
    pub(crate) fn is_async_module_state(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.async_module_state)
                .is_ok()
        })
    }

    pub(crate) fn complete_async_module_await_resolution(
        &mut self,
        state_value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.async_module_state_reference(state_value)?;
        let paused = core::mem::take(&mut self.fiber);
        self.fiber = self.publish_async_module_pause(state, paused)?;
        Ok(())
    }

    fn async_module_state_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<AsyncModuleState>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        self.heap
            .checked_reference(raw, self.types.async_module_state)
            .map_err(|_| ExecutionError::UnsupportedAsyncFunctionResume)
    }

    fn prepare_async_module_await(
        &mut self,
        state: GcRef<AsyncModuleState>,
        destination: u32,
        instruction: WordOffset,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.async_module_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if state.paused.is_some() || state.await_destination.is_some() {
                    return Err(ExecutionError::UnsupportedAsyncFunctionResume);
                }
                state.await_destination = Some(destination);
                state.await_instruction = Some(instruction);
                Ok(())
            })
        })
    }

    fn publish_async_module_pause(
        &mut self,
        state: GcRef<AsyncModuleState>,
        paused: Fiber,
    ) -> Result<Fiber, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.async_module_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.paused = Some(paused);
                state
                    .caller
                    .take()
                    .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)
            })
        })
    }

    fn take_async_module_pause(
        &mut self,
        state: GcRef<AsyncModuleState>,
        caller: Fiber,
    ) -> Result<(Fiber, u32, WordOffset), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.async_module_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.caller = Some(caller);
                Ok((
                    state
                        .paused
                        .take()
                        .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?,
                    state
                        .await_destination
                        .take()
                        .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?,
                    state
                        .await_instruction
                        .take()
                        .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?,
                ))
            })
        })
    }

    fn async_module_id(
        &mut self,
        state: GcRef<AsyncModuleState>,
    ) -> Result<ModuleId, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.async_module_state)
                    .map(|state| state.module)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn restore_async_module_caller(
        &mut self,
        state: GcRef<AsyncModuleState>,
    ) -> Result<(), ExecutionError> {
        let caller = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.async_module_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .caller
                    .take()
                    .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)
            })
        })?;
        self.fiber = caller;
        Ok(())
    }
}
