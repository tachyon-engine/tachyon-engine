//! GC-owned async-function activation and `await` Fiber transfer state.

use super::*;

#[derive(Clone, Copy, Debug)]
struct AsyncFunctionActivation {
    environment: Option<GcRef<Environment>>,
    this_value: Value,
    callee: Value,
    argument_prefix: GcRef<BoundFunctionData>,
    argument_count: u32,
}

impl Trace for AsyncFunctionActivation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.environment.trace(tracer);
        self.this_value.trace(tracer);
        self.callee.trace(tracer);
        self.argument_prefix.trace(tracer);
    }
}

/// Hidden state owns exactly one async activation and its caller/paused Fiber pair.
pub(crate) struct AsyncFunctionState {
    promise: Value,
    activation: Option<AsyncFunctionActivation>,
    caller: Option<Fiber>,
    paused: Option<Fiber>,
    await_destination: Option<u32>,
    await_instruction: Option<WordOffset>,
}

impl Trace for AsyncFunctionState {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
        self.activation.trace(tracer);
        if let Some(caller) = &mut self.caller {
            caller.trace_roots(tracer);
        }
        if let Some(paused) = &mut self.paused {
            paused.trace_roots(tracer);
        }
    }
}

struct AsyncFunctionAllocationRoots<'a> {
    vm: VmRoots<'a>,
    promise: Value,
    activation: AsyncFunctionActivation,
}

impl Trace for AsyncFunctionAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.promise.trace(tracer);
        self.activation.trace(tracer);
    }
}

pub(crate) struct AsyncAwaitSite {
    pub(crate) code: CodeId,
    pub(crate) instruction: WordOffset,
    pub(crate) source: u32,
    pub(crate) destination: u32,
    pub(crate) suspend_id: u32,
    pub(crate) base: u32,
}

impl Isolate {
    /// Starts an async function on a child Fiber while publishing its result Promise to the caller.
    pub(crate) fn begin_async_function(
        &mut self,
        site: &CallSite,
        target: ResolvedCallTarget,
    ) -> Result<(), ExecutionError> {
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(site.argument_count as usize)
            .map_err(|_| ExecutionError::AsyncFunctionArgumentAllocationFailed)?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .expect("async call argument remains in the call view"),
            );
        }
        let this_value = self.bind_ordinary_this(target.strictness, site.this_value);
        let argument_prefix =
            self.create_apply_argument_prefix(site.callee, this_value, arguments)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(argument_prefix.raw()),
        )?;
        let promise = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        let activation = AsyncFunctionActivation {
            environment: target.environment,
            this_value,
            callee: site.callee,
            argument_prefix,
            argument_count: site.argument_count,
        };
        let mut roots = AsyncFunctionAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            promise,
            activation,
        };
        let state = self
            .heap
            .try_allocate_with_gc(
                self.types.async_function_state,
                0,
                0,
                AsyncFunctionState {
                    promise: roots.promise,
                    activation: Some(roots.activation),
                    caller: None,
                    paused: None,
                    await_destination: None,
                    await_instruction: None,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let state_value = Value::from_heap_ref(state.raw());
        self.write(site.caller_base, site.destination, promise)?;

        let caller = core::mem::take(&mut self.fiber);
        self.fiber
            .completions
            .set_limit(self.stack_limits.max_completions);
        self.fiber
            .completions
            .push_native(NativeContinuation::async_function(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                state_value,
            ))
            .map_err(Self::completion_stack_error)?;
        self.set_async_function_caller(state, caller)?;
        self.push_call_frame(
            target,
            CallSite {
                caller_base: site.caller_base,
                destination: site.destination,
                callee: activation.callee,
                argument_base: 0,
                argument_source: None,
                argument_prefix: Some(activation.argument_prefix),
                argument_prefix_offset: 0,
                argument_prefix_count: activation.argument_count,
                argument_count: activation.argument_count,
                this_value: activation.this_value,
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: site.call_site,
            },
        )?;
        self.fiber
            .frames
            .last_mut()
            .expect("async start publishes one bytecode frame")
            .return_continuation = true;
        self.clear_async_function_activation(state)?;
        Ok(())
    }

    /// Verifies `Await` metadata, attaches internal reactions, and restores the caller Fiber.
    pub(crate) fn suspend_async_function_await(
        &mut self,
        site: AsyncAwaitSite,
    ) -> Result<(), ExecutionError> {
        let frame = self
            .fiber
            .frames
            .last()
            .copied()
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        let continuation_index = frame
            .completion_base
            .checked_sub(1)
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?
            as usize;
        let continuation = self
            .fiber
            .completions
            .native_at(continuation_index)
            .filter(|continuation| continuation.kind() == NativeContinuationKind::AsyncFunction)
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        let state_value = continuation.first();
        let state = self.async_function_state_reference(state_value)?;
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
        let source = self.read(site.base, site.source)?;
        if self.promise_snapshot(source).is_ok() {
            self.prepare_async_function_await(state, site.destination, site.instruction)?;
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
        if !self.is_object_value(source) {
            let awaited = self.create_promise(PromiseState::Fulfilled, source)?;
            self.perform_promise_then_with_capability(awaited, None, None, state_value)?;
            self.prepare_async_function_await(state, site.destination, site.instruction)?;
            return self.complete_async_await_resolution();
        }
        let awaited = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.perform_promise_then_with_capability(awaited, None, None, state_value)?;
        self.prepare_async_function_await(state, site.destination, site.instruction)?;
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
    }

    /// Performs the observable PromiseResolve constructor lookup for an awaited native Promise.
    pub(crate) fn begin_async_await_constructor_get(
        &mut self,
        continuation_site: NativeContinuationSite,
        state: Value,
        source: Value,
    ) -> Result<(), ExecutionError> {
        let continuation =
            NativeContinuation::async_await_constructor(continuation_site, state, source);
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let constructor = self.constructor_atom()?;
        let saved_destination;
        let result = match self.resolve_property_read_until_proxy(source, constructor.into())? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.pop_native_continuation()?;
                return self.resume_async_await_constructor(
                    continuation,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.pop_native_continuation()?;
                return self.resume_async_await_constructor(continuation, value);
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.pop_native_continuation()?;
                return self.resume_async_await_constructor(
                    continuation,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => {
                self.pop_native_continuation()?;
                return self
                    .dispatch_property_callback(continuation, getter)
                    .map(|_| ());
            }
            PropertyReadResolution::Proxy(_) => {
                saved_destination =
                    Some(self.read(continuation_site.caller_base, continuation_site.destination)?);
                self.dispatch_proxy_aware_property_read(
                    continuation_site,
                    source,
                    source,
                    constructor.into(),
                )
            }
        };
        if let Err(error) = result {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            if let Some(saved) = saved_destination {
                self.write(
                    continuation_site.caller_base,
                    continuation_site.destination,
                    saved,
                )?;
            }
            return self.reject_async_await_constructor_error(continuation, error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let constructor =
            self.read(continuation_site.caller_base, continuation_site.destination)?;
        if let Some(saved) = saved_destination {
            self.write(
                continuation_site.caller_base,
                continuation_site.destination,
                saved,
            )?;
        }
        self.resume_async_await_constructor(continuation, constructor)
    }

    /// Selects the PromiseResolve identity path or creates the intrinsic wrapper Promise.
    pub(crate) fn resume_async_await_constructor(
        &mut self,
        continuation: NativeContinuation,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        let source = continuation.second();
        let state = continuation.first();
        let intrinsic = self
            .realm
            .promise_constructor
            .expect("Promise constructor initializes before Await");
        let awaiting_return = self.is_async_generator_awaiting_return(state);
        if constructor == intrinsic {
            self.perform_promise_then_with_capability(source, None, None, state)?;
            if awaiting_return {
                return Ok(());
            }
            return self.complete_async_await_resolution();
        }
        let awaited = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.perform_promise_then_with_capability(awaited, None, None, state)?;
        self.begin_promise_resolution(
            awaited,
            source,
            continuation.site(),
            if awaiting_return {
                PromiseResolutionMode::StaticResolve
            } else {
                PromiseResolutionMode::AsyncAwait
            },
        )
    }

    /// Converts an abrupt Promise constructor lookup into the rejected Promise awaited by the fiber.
    pub(crate) fn reject_async_await_constructor(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let awaited = self.create_promise(PromiseState::Rejected, reason)?;
        self.perform_promise_then_with_capability(awaited, None, None, continuation.first())?;
        if self.is_async_generator_awaiting_return(continuation.first()) {
            return Ok(());
        }
        self.complete_async_await_resolution()
    }

    /// Maps an internal VM error from the constructor lookup to its JavaScript rejection value.
    fn reject_async_await_constructor_error(
        &mut self,
        continuation: NativeContinuation,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let reason = match error {
            ExecutionError::HostThrown(value) => value,
            error => {
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                self.create_native_error(kind, None)?
            }
        };
        self.reject_async_await_constructor(continuation, reason)
    }

    /// Publishes the active child once PromiseResolve has produced the awaited Promise.
    pub(crate) fn complete_async_await_resolution(&mut self) -> Result<(), ExecutionError> {
        let frame = self
            .fiber
            .frames
            .last()
            .copied()
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        let continuation_index = frame
            .completion_base
            .checked_sub(1)
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?
            as usize;
        let continuation = self
            .fiber
            .completions
            .native_at(continuation_index)
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        if continuation.kind() == NativeContinuationKind::GeneratorResume
            && continuation.second().as_immediate() != Some(Immediate::Undefined)
        {
            let generator = self.generator_reference(continuation.first())?;
            return self.complete_async_generator_await_resolution(generator);
        }
        if continuation.kind() != NativeContinuationKind::AsyncFunction {
            return Err(ExecutionError::UnsupportedAsyncFunctionResume);
        }
        let state = self.async_function_state_reference(continuation.first())?;
        let paused = core::mem::take(&mut self.fiber);
        self.fiber = self.publish_prepared_async_function_pause(state, paused)?;
        Ok(())
    }

    /// Resumes one awaited completion from the Promise checkpoint, never synchronously.
    pub(crate) fn resume_async_function_job(
        &mut self,
        state_value: Value,
        argument: Value,
        rejected: bool,
        return_site: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.async_function_state_reference(state_value)?;
        self.fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?
            .pc = return_site;
        let caller = core::mem::take(&mut self.fiber);
        let (paused, destination, instruction) = self.take_async_function_pause(state, caller)?;
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

    /// Settles the async result Promise and restores the caller without exposing the body value.
    pub(crate) fn finish_async_function_return(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.async_function_state_reference(continuation.first())?;
        let promise = self.async_function_promise(state)?;
        self.settle_promise(promise, PromiseState::Fulfilled, value)?;
        self.restore_async_function_caller(state)
    }

    /// Converts an uncaught async-body throw into rejection and restores its caller Fiber.
    pub(crate) fn finish_async_function_throw(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.async_function_state_reference(continuation.first())?;
        let promise = self.async_function_promise(state)?;
        self.settle_promise(promise, PromiseState::Rejected, reason)?;
        self.restore_async_function_caller(state)
    }

    #[inline]
    pub(crate) fn is_async_function_state(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.async_function_state)
                .is_ok()
        })
    }

    fn async_function_state_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<AsyncFunctionState>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
        self.heap
            .checked_reference(raw, self.types.async_function_state)
            .map_err(|_| ExecutionError::UnsupportedAsyncFunctionResume)
    }

    fn set_async_function_caller(
        &mut self,
        state: GcRef<AsyncFunctionState>,
        caller: Fiber,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.async_function_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.caller = Some(caller);
                Ok(())
            })
        })
    }

    fn clear_async_function_activation(
        &mut self,
        state: GcRef<AsyncFunctionState>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.async_function_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .activation = None;
                Ok(())
            })
        })
    }

    /// Records verified metadata before PromiseResolve can execute observable JavaScript.
    fn prepare_async_function_await(
        &mut self,
        state: GcRef<AsyncFunctionState>,
        destination: u32,
        instruction: WordOffset,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.async_function_state)
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

    /// Publishes the prepared child and atomically takes ownership of its caller.
    fn publish_prepared_async_function_pause(
        &mut self,
        state: GcRef<AsyncFunctionState>,
        paused: Fiber,
    ) -> Result<Fiber, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.async_function_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if state.paused.is_some()
                    || state.await_destination.is_none()
                    || state.await_instruction.is_none()
                {
                    return Err(ExecutionError::UnsupportedAsyncFunctionResume);
                }
                state.paused = Some(paused);
                state
                    .caller
                    .take()
                    .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)
            })
        })
    }

    /// Exchanges a checkpoint caller for the paused async child and its resume metadata.
    fn take_async_function_pause(
        &mut self,
        state: GcRef<AsyncFunctionState>,
        caller: Fiber,
    ) -> Result<(Fiber, u32, WordOffset), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.async_function_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if state.caller.is_some() {
                    return Err(ExecutionError::UnsupportedAsyncFunctionResume);
                }
                state.caller = Some(caller);
                let paused = state
                    .paused
                    .take()
                    .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
                let destination = state
                    .await_destination
                    .take()
                    .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
                let instruction = state
                    .await_instruction
                    .take()
                    .ok_or(ExecutionError::UnsupportedAsyncFunctionResume)?;
                Ok((paused, destination, instruction))
            })
        })
    }

    fn async_function_promise(
        &mut self,
        state: GcRef<AsyncFunctionState>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.async_function_state)
                    .map(|state| state.promise)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn restore_async_function_caller(
        &mut self,
        state: GcRef<AsyncFunctionState>,
    ) -> Result<(), ExecutionError> {
        let caller = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.async_function_state)
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
