//! Generator instance state and the retained call payload used by first resume.

use std::collections::VecDeque;

use tachyon_gc::{GcRef, Trace, Tracer};
use tachyon_value::Value;

use crate::{
    AllocationSpace, ExecutionError, FunctionKind, Immediate, Isolate, NativeErrorKind, ShapeId,
    bound_function::BoundFunctionData,
    object::OrdinaryObject,
    runtime::{
        callable::{CallSite, ResolvedCallTarget},
        code::CodeId,
        completion::{CompletionKind, CompletionRecord},
        environment::Environment,
        fiber::{Fiber, NativeContinuation, NativeContinuationSite, VmRoots},
    },
};
use tachyon_bytecode::{FunctionId, WordOffset};

#[derive(Clone, Copy, Debug)]
enum GeneratorResume {
    Next(Value),
    Abrupt(CompletionRecord),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncGeneratorRequestKind {
    Next,
    Return,
    Throw,
}

#[derive(Clone, Copy, Debug)]
struct AsyncGeneratorRequest {
    promise: Value,
    value: Value,
    call_site: WordOffset,
    kind: AsyncGeneratorRequestKind,
}

impl Trace for AsyncGeneratorRequest {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
        self.value.trace(tracer);
    }
}

#[derive(Debug)]
struct FiberTransferError {
    error: ExecutionError,
    fiber: Fiber,
}

#[derive(Clone, Copy, Debug)]
struct InjectedGeneratorAbrupt {
    completion: CompletionRecord,
    instruction: WordOffset,
}

#[derive(Debug)]
struct PreparedGeneratorResume {
    fiber: Fiber,
    abrupt: Option<InjectedGeneratorAbrupt>,
}

/// Complete bytecode location and register contract for one generator suspension.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GeneratorSuspendSite {
    pub(crate) code: CodeId,
    pub(crate) instruction: WordOffset,
    pub(crate) source: u32,
    pub(crate) destination: u32,
    pub(crate) kind_destination: Option<u32>,
    pub(crate) suspend_id: u32,
    pub(crate) base: u32,
}

/// Spec-visible ordinary generator execution state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum GeneratorState {
    SuspendedStart,
    SuspendedYield,
    Executing,
    AwaitingReturn,
    Completed,
}

/// Fixed-size roots retained only until the first bytecode frame is successfully published.
#[derive(Clone, Copy, Debug)]
struct GeneratorActivation {
    environment: Option<GcRef<Environment>>,
    this_value: Value,
    callee: Value,
    argument_prefix: GcRef<BoundFunctionData>,
    argument_count: u32,
}

impl Trace for GeneratorActivation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.environment.trace(tracer);
        self.this_value.trace(tracer);
        self.callee.trace(tracer);
        self.argument_prefix.trace(tracer);
    }
}

/// GC-managed generator instance with a one-shot retained activation.
///
/// The first vertical slice retains the initial activation inputs until `.next()` publishes the
/// explicit bytecode frame. A later yield implementation will add a paused-fiber handle without
/// changing the public state machine or replaying the function body.
#[derive(Debug)]
pub(crate) struct GeneratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) code: CodeId,
    pub(crate) function: FunctionId,
    activation: Option<GeneratorActivation>,
    caller: Option<Fiber>,
    paused: Option<Fiber>,
    resume_destination: Option<u32>,
    resume_kind_destination: Option<u32>,
    resume_instruction: Option<WordOffset>,
    async_requests: VecDeque<AsyncGeneratorRequest>,
    active_async_request: Option<AsyncGeneratorRequest>,
    is_async: bool,
    pub(crate) state: GeneratorState,
}

impl GeneratorObject {
    /// Creates one suspended-start activation with fixed backing and no hidden capacity.
    fn new(
        ordinary: OrdinaryObject,
        code: CodeId,
        function: FunctionId,
        activation: GeneratorActivation,
        is_async: bool,
    ) -> Self {
        Self {
            ordinary,
            code,
            function,
            activation: Some(activation),
            caller: None,
            paused: None,
            resume_destination: None,
            resume_kind_destination: None,
            resume_instruction: None,
            async_requests: VecDeque::with_capacity(
                crate::tuning::promises::INITIAL_ASYNC_GENERATOR_REQUEST_CAPACITY,
            ),
            active_async_request: None,
            is_async,
            state: GeneratorState::SuspendedStart,
        }
    }

    #[inline(always)]
    fn header(&self) -> GeneratorHeader {
        GeneratorHeader {
            code: self.code,
            function: self.function,
            state: self.state,
            is_async: self.is_async,
        }
    }
}

impl Trace for GeneratorObject {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.activation.trace(tracer);
        for request in &mut self.async_requests {
            request.trace(tracer);
        }
        self.active_async_request.trace(tracer);
        if let Some(caller) = &mut self.caller {
            caller.trace_roots(tracer);
        }
        if let Some(paused) = &mut self.paused {
            paused.trace_roots(tracer);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct GeneratorHeader {
    code: CodeId,
    function: FunctionId,
    state: GeneratorState,
    is_async: bool,
}

impl Isolate {
    /// Captures a generator call without entering its body or publishing a JavaScript frame.
    pub(crate) fn create_generator_from_site(
        &mut self,
        site: &CallSite,
        target: ResolvedCallTarget,
    ) -> Result<Value, ExecutionError> {
        let prototype = self.ensure_function_prototype(site.callee)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(site.argument_count as usize)
            .map_err(|_| ExecutionError::GeneratorArgumentAllocationFailed)?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .expect("generator call argument remains in the call view"),
            );
        }
        let this_value = self.bind_ordinary_this(target.strictness, site.this_value);
        let argument_prefix =
            self.create_apply_argument_prefix(site.callee, this_value, arguments)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.generator_object,
                0,
                0,
                GeneratorObject::new(
                    OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                    target.code,
                    target.function,
                    GeneratorActivation {
                        environment: target.environment,
                        this_value,
                        callee: site.callee,
                        argument_prefix,
                        argument_count: site.argument_count,
                    },
                    target.kind == FunctionKind::AsyncGenerator,
                ),
                AllocationSpace::Young,
                roots,
            )
            .map(|generator| Value::from_heap_ref(generator.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Starts a suspended generator on the existing iterative frame machine.
    pub(crate) fn begin_generator_next(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_generator_next_kind(site, false, None)
    }

    /// Enqueues one async-generator next request and returns its Promise before body execution.
    pub(crate) fn begin_async_generator_next(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.enqueue_async_generator_request(site, AsyncGeneratorRequestKind::Next)
    }

    /// Enqueues one async-generator return request without synchronously exposing its completion.
    pub(crate) fn begin_async_generator_return(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.enqueue_async_generator_request(site, AsyncGeneratorRequestKind::Return)
    }

    /// Enqueues one async-generator throw request for ordered rejection or abrupt resumption.
    pub(crate) fn begin_async_generator_throw(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.enqueue_async_generator_request(site, AsyncGeneratorRequestKind::Throw)
    }

    /// Publishes a pending Promise, appends the traced request, and starts only an idle queue.
    fn enqueue_async_generator_request(
        &mut self,
        site: &CallSite,
        kind: AsyncGeneratorRequestKind,
    ) -> Result<(), ExecutionError> {
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.write(site.caller_base, site.destination, site.this_value)?;
        let promise = self.create_promise(
            crate::promise_state::PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        let generator_value = self.read(site.caller_base, site.destination)?;
        self.write(site.caller_base, site.destination, promise)?;
        let generator = match self.generator_reference(generator_value) {
            Ok(generator) if self.generator_header(generator)?.is_async => generator,
            Ok(_) | Err(ExecutionError::GeneratorBrand(_)) => {
                let reason = self.create_native_error(NativeErrorKind::Type, None)?;
                self.settle_promise(
                    promise,
                    crate::promise_state::PromiseState::Rejected,
                    reason,
                )?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let request = AsyncGeneratorRequest {
            promise,
            value,
            call_site: site.call_site,
            kind,
        };
        let should_start = self.push_async_generator_request(generator, request)?;
        if should_start {
            self.resume_next_async_generator_request(generator_value)?;
        }
        Ok(())
    }

    /// Appends one request under a no-GC borrow and publishes both young edges with barriers.
    fn push_async_generator_request(
        &mut self,
        generator: GcRef<GeneratorObject>,
        request: AsyncGeneratorRequest,
    ) -> Result<bool, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            let should_start = scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                generator
                    .async_requests
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::GeneratorArgumentAllocationFailed)?;
                let should_start = generator.active_async_request.is_none()
                    && !matches!(
                        generator.state,
                        GeneratorState::Executing | GeneratorState::AwaitingReturn
                    );
                generator.async_requests.push_back(request);
                Ok(should_start)
            })?;
            scope
                .write_value_barrier(generator, request.promise)
                .map_err(ExecutionError::HeapReference)?;
            scope
                .write_value_barrier(generator, request.value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(should_start)
        })
    }

    /// Drains inactive/completed requests and starts at most one executable generator request.
    fn resume_next_async_generator_request(
        &mut self,
        generator_value: Value,
    ) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(generator_value)?;
        loop {
            let request = self.activate_async_generator_request(generator)?;
            let header = self.generator_header(generator)?;
            match (header.state, request.kind) {
                (GeneratorState::Executing | GeneratorState::AwaitingReturn, _) => return Ok(()),
                (GeneratorState::Completed, AsyncGeneratorRequestKind::Next) => {
                    let result = self.create_iterator_result(
                        Value::from_immediate(Immediate::Undefined),
                        true,
                    )?;
                    self.settle_active_async_generator_request(generator, result, false)?;
                }
                (GeneratorState::Completed, AsyncGeneratorRequestKind::Return)
                | (GeneratorState::SuspendedStart, AsyncGeneratorRequestKind::Return) => {
                    if header.state == GeneratorState::SuspendedStart {
                        self.complete_generator_without_resume(generator)?;
                    }
                    return self.begin_async_generator_await_return(
                        generator_value,
                        generator,
                        request.value,
                    );
                }
                (GeneratorState::Completed, AsyncGeneratorRequestKind::Throw)
                | (GeneratorState::SuspendedStart, AsyncGeneratorRequestKind::Throw) => {
                    if header.state == GeneratorState::SuspendedStart {
                        self.complete_generator_without_resume(generator)?;
                    }
                    self.settle_active_async_generator_request(generator, request.value, true)?;
                }
                (_, AsyncGeneratorRequestKind::Next) => {
                    let site = self.async_generator_request_site(generator_value, request);
                    return self.begin_generator_next_kind(&site, true, Some(request.value));
                }
                (GeneratorState::SuspendedYield, AsyncGeneratorRequestKind::Return) => {
                    let site = self.async_generator_request_site(generator_value, request);
                    return self.begin_generator_abrupt(
                        &site,
                        CompletionRecord::return_value(request.value),
                        true,
                    );
                }
                (_, AsyncGeneratorRequestKind::Throw) => {
                    let site = self.async_generator_request_site(generator_value, request);
                    return self.begin_generator_abrupt(
                        &site,
                        CompletionRecord::throw(request.value),
                        true,
                    );
                }
            }
            if !self.has_queued_async_generator_request(generator)? {
                return Ok(());
            }
        }
    }

    /// Awaits a queued return value after the generator body can no longer execute.
    fn begin_async_generator_await_return(
        &mut self,
        generator_value: Value,
        generator: GcRef<GeneratorObject>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_generator_state(generator, GeneratorState::AwaitingReturn)?;
        let frame = self
            .fiber
            .frames
            .last()
            .copied()
            .ok_or(ExecutionError::MissingEnvironment)?;
        let site = NativeContinuationSite {
            caller_base: frame.base,
            destination: 0,
            call_site: frame.pc,
        };
        if self.promise_snapshot(value).is_ok() {
            return self.begin_async_await_constructor_get(site, generator_value, value);
        }
        if !self.is_object_value(value) {
            let awaited =
                self.create_promise(crate::promise_state::PromiseState::Fulfilled, value)?;
            return self.perform_promise_then_with_capability(awaited, None, None, generator_value);
        }
        let awaited = self.create_promise(
            crate::promise_state::PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.perform_promise_then_with_capability(awaited, None, None, generator_value)?;
        self.begin_promise_resolution(
            awaited,
            value,
            site,
            crate::runtime::fiber::PromiseResolutionMode::StaticResolve,
        )
    }

    /// Builds an allocation-free internal call site; async results settle the active Promise.
    fn async_generator_request_site(
        &self,
        generator: Value,
        request: AsyncGeneratorRequest,
    ) -> CallSite {
        CallSite {
            caller_base: self.fiber.frames.last().map_or(0, |frame| frame.base),
            destination: 0,
            callee: generator,
            argument_base: 0,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: generator,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: self
                .fiber
                .frames
                .last()
                .map_or(request.call_site, |frame| frame.pc),
        }
    }

    /// Starts or resumes one generator after its sync/async brand has been checked.
    fn begin_generator_next_kind(
        &mut self,
        site: &CallSite,
        expected_async: bool,
        resume_value: Option<Value>,
    ) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(site.this_value)?;
        let header = self.generator_header(generator)?;
        if header.is_async != expected_async {
            return Err(ExecutionError::GeneratorBrand(site.this_value));
        }
        match header.state {
            GeneratorState::Completed => {
                let result =
                    self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
                return self.write(site.caller_base, site.destination, result);
            }
            GeneratorState::Executing | GeneratorState::AwaitingReturn => {
                return Err(ExecutionError::GeneratorExecuting);
            }
            GeneratorState::SuspendedYield => {
                return self.resume_generator_yield(generator, site, resume_value);
            }
            GeneratorState::SuspendedStart => {}
        }
        let activation = self.generator_activation(generator)?;
        let (layout, strictness) = {
            let function = self
                .loaded_code(header.code)?
                .module
                .function(header.function)
                .ok_or(ExecutionError::MissingEntryFunction(header.function))?;
            (function.layout(), function.strictness())
        };
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let continuation = if header.is_async {
            let request = self.active_async_generator_request(generator)?;
            NativeContinuation::async_generator_resume(
                continuation_site,
                site.this_value,
                request.promise,
            )
        } else {
            NativeContinuation::generator_resume(continuation_site, site.this_value)
        };
        let caller_fiber = core::mem::take(&mut self.fiber);
        if let Err(rollback) = self.set_generator_caller(generator, caller_fiber) {
            self.fiber = rollback.fiber;
            return Err(rollback.error);
        }
        self.fiber
            .completions
            .set_limit(self.stack_limits.max_completions);
        let pushed = self
            .fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)
            .and_then(|()| {
                self.set_generator_state(generator, GeneratorState::Executing)?;
                self.push_call_frame(
                    ResolvedCallTarget {
                        code: header.code,
                        function: header.function,
                        environment: activation.environment,
                        kind: if header.is_async {
                            FunctionKind::AsyncGenerator
                        } else {
                            FunctionKind::Generator
                        },
                        layout,
                        strictness,
                    },
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
                )
            });
        if let Err(error) = pushed {
            let _ = self.fiber.completions.pop_native();
            self.set_generator_state(generator, GeneratorState::SuspendedStart)?;
            self.restore_generator_caller_fiber(generator)?;
            return Err(error);
        }
        self.clear_generator_activation(generator)?;
        self.fiber
            .frames
            .last_mut()
            .expect("generator resume publishes one bytecode frame")
            .return_continuation = true;
        Ok(())
    }

    /// Injects a Return completion into a suspended generator or completes an inactive one.
    pub(crate) fn begin_generator_return(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.begin_generator_abrupt(site, CompletionRecord::return_value(value), false)
    }

    /// Injects a Throw completion into a suspended generator or throws from an inactive one.
    pub(crate) fn begin_generator_throw(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.begin_generator_abrupt(site, CompletionRecord::throw(value), false)
    }

    /// Implements GeneratorResumeAbrupt without bypassing the existing catch/finally dispatcher.
    fn begin_generator_abrupt(
        &mut self,
        site: &CallSite,
        completion: CompletionRecord,
        expected_async: bool,
    ) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(site.this_value)?;
        let header = self.generator_header(generator)?;
        if header.is_async != expected_async {
            return Err(ExecutionError::GeneratorBrand(site.this_value));
        }
        if matches!(
            header.state,
            GeneratorState::Executing | GeneratorState::AwaitingReturn
        ) {
            return Err(ExecutionError::GeneratorExecuting);
        }
        if matches!(
            header.state,
            GeneratorState::SuspendedStart | GeneratorState::Completed
        ) {
            if header.state == GeneratorState::SuspendedStart {
                self.complete_generator_without_resume(generator)?;
            }
            return self.finish_inactive_generator_abrupt(site, completion);
        }
        self.resume_generator_abrupt(generator, site, completion, expected_async)
    }

    /// Produces the state-independent observable result after no generator code can execute.
    fn finish_inactive_generator_abrupt(
        &mut self,
        site: &CallSite,
        completion: CompletionRecord,
    ) -> Result<(), ExecutionError> {
        let value = completion
            .value()
            .ok_or(ExecutionError::MissingCompletionRecord)?;
        match completion.kind() {
            CompletionKind::Return => {
                let result = self.create_iterator_result(value, true)?;
                self.write(site.caller_base, site.destination, result)
            }
            CompletionKind::Throw => Err(ExecutionError::HostThrown(value)),
            _ => Err(ExecutionError::MissingCompletionRecord),
        }
    }

    /// Completes a generator return before creating the observable iterator result object.
    pub(crate) fn finish_generator_return(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(continuation.first())?;
        self.set_generator_state(generator, GeneratorState::Completed)?;
        self.restore_generator_caller_fiber(generator)?;
        let result = self.create_iterator_result(value, true)?;
        if continuation.second().as_immediate() != Some(Immediate::Undefined) {
            let generator = self.generator_reference(continuation.first())?;
            return self.schedule_async_generator_settlement(generator, result, false);
        }
        let site = continuation.site();
        self.write(site.caller_base, site.destination, result)
    }

    /// Marks an abruptly completed generator before the original throw continues in its caller.
    pub(crate) fn finish_generator_throw(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<bool, ExecutionError> {
        let generator = self.generator_reference(continuation.first())?;
        self.set_generator_state(generator, GeneratorState::Completed)?;
        self.restore_generator_caller_fiber(generator)?;
        if continuation.second().as_immediate() == Some(Immediate::Undefined) {
            return Ok(false);
        }
        let generator = self.generator_reference(continuation.first())?;
        self.schedule_async_generator_settlement(generator, reason, true)?;
        Ok(true)
    }

    pub(crate) fn generator_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<GeneratorObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::GeneratorBrand(value))?;
        self.heap
            .checked_reference(raw, self.types.generator_object)
            .map_err(|_| ExecutionError::GeneratorBrand(value))
    }

    /// Reads only the compact execution header used by every `.next()` state branch.
    fn generator_header(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<GeneratorHeader, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(generator.header())
            })
        })
    }

    /// Copies the fixed activation header only for a verified suspended-start generator.
    fn generator_activation(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<GeneratorActivation, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .activation
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        })
    }

    /// Moves the FIFO head into the separately traced active request slot exactly once.
    fn activate_async_generator_request(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<AsyncGeneratorRequest, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if let Some(request) = generator.active_async_request {
                    return Ok(request);
                }
                let request = generator
                    .async_requests
                    .pop_front()
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
                generator.active_async_request = Some(request);
                Ok(request)
            })
        })
    }

    /// Snapshots the active Promise request while the generator Fiber owns execution.
    fn active_async_generator_request(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<AsyncGeneratorRequest, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .active_async_request
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        })
    }

    /// Removes and settles the active request after its iterator result is fully allocated.
    fn settle_active_async_generator_request(
        &mut self,
        generator: GcRef<GeneratorObject>,
        result: Value,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        let promise = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .active_async_request
                    .take()
                    .map(|request| request.promise)
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        })?;
        self.settle_promise(
            promise,
            if rejected {
                crate::promise_state::PromiseState::Rejected
            } else {
                crate::promise_state::PromiseState::Fulfilled
            },
            result,
        )
    }

    /// Defers iterator-result settlement so all same-job requests enter the generator FIFO first.
    fn schedule_async_generator_settlement(
        &mut self,
        generator: GcRef<GeneratorObject>,
        result: Value,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        let request = self.active_async_generator_request(generator)?;
        self.promise_jobs
            .push(crate::promise_state::PromiseJob::AsyncGeneratorSettlement {
                generator: Value::from_heap_ref(generator.raw()),
                promise: request.promise,
                result,
                rejected,
            });
        Ok(())
    }

    /// Settles one queued job, removes its request, then starts the next request if present.
    pub(crate) fn finish_async_generator_settlement(
        &mut self,
        generator_value: Value,
        promise: Value,
        result: Value,
        rejected: bool,
        checkpoint_site: WordOffset,
    ) -> Result<bool, ExecutionError> {
        let generator = self.generator_reference(generator_value)?;
        let active = self.active_async_generator_request(generator)?;
        if active.promise != promise {
            return Err(ExecutionError::UnsupportedGeneratorYieldResume);
        }
        self.settle_active_async_generator_request(generator, result, rejected)?;
        self.promise_jobs.finish_active();
        if !self.has_queued_async_generator_request(generator)? {
            return Ok(false);
        }
        // A top-level Return may fall through to bytecode-end. Resume its caller at the Return
        // instruction so the idempotent Promise checkpoint continues after generator suspension.
        self.fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?
            .pc = checkpoint_site;
        self.resume_next_async_generator_request(generator_value)?;
        Ok(true)
    }

    /// Reports whether a settled async generator has another FIFO request to consume.
    fn has_queued_async_generator_request(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<bool, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(generator, self.types.generator_object)
                    .map(|generator| !generator.async_requests.is_empty())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Closes a suspended-start generator and releases roots without executing its body.
    fn complete_generator_without_resume(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if generator.state != GeneratorState::SuspendedStart
                    || generator.caller.is_some()
                    || generator.paused.is_some()
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                generator.activation = None;
                generator.state = GeneratorState::Completed;
                Ok(())
            })
        })
    }

    /// Stores the caller in the generator so every allocation safepoint traces the suspended roots.
    #[allow(
        clippy::result_large_err,
        reason = "the allocation-free rollback path must return full Fiber ownership"
    )]
    fn set_generator_caller(
        &mut self,
        generator: GcRef<GeneratorObject>,
        caller: Fiber,
    ) -> Result<(), FiberTransferError> {
        let mut caller = Some(caller);
        let result = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if generator.caller.is_some() {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                generator.caller = caller.take();
                Ok(())
            })
        });
        result.map_err(|error| FiberTransferError {
            error,
            fiber: caller.expect("failed caller publication retains fiber ownership"),
        })
    }

    /// Takes the caller only after generator execution has yielded or completed.
    fn take_generator_caller(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<Fiber, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .caller
                    .take()
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        })
    }

    /// Moves a suspended generator fiber back into the isolate and drops the empty execution fiber.
    fn restore_generator_caller_fiber(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<(), ExecutionError> {
        let caller = self.take_generator_caller(generator)?;
        let _generator_fiber = core::mem::replace(&mut self.fiber, caller);
        Ok(())
    }

    /// Awaits one value without settling or removing the active async-generator request.
    pub(crate) fn suspend_async_generator_await(
        &mut self,
        site: crate::async_function::AsyncAwaitSite,
    ) -> Result<(), ExecutionError> {
        let frame = self
            .fiber
            .frames
            .last()
            .copied()
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        let continuation_index = frame
            .completion_base
            .checked_sub(1)
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?
            as usize;
        let continuation = self
            .fiber
            .completions
            .native_at(continuation_index)
            .filter(|continuation| {
                continuation.kind()
                    == crate::runtime::fiber::NativeContinuationKind::GeneratorResume
                    && continuation.second().as_immediate() != Some(Immediate::Undefined)
            })
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        let generator_value = continuation.first();
        let generator = self.generator_reference(generator_value)?;
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
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        if point.instruction != site.instruction
            || point.destination.index() != site.destination
            || point.resume_offset != frame.pc
        {
            return Err(ExecutionError::UnsupportedGeneratorYieldResume);
        }
        self.prepare_async_generator_await(generator, site.destination, site.instruction)?;
        let source = self.read(site.base, site.source)?;
        if self.promise_snapshot(source).is_ok() {
            return self.begin_async_await_constructor_get(
                NativeContinuationSite {
                    caller_base: site.base,
                    destination: site.destination,
                    call_site: site.instruction,
                },
                generator_value,
                source,
            );
        }
        if !self.is_object_value(source) {
            let awaited =
                self.create_promise(crate::promise_state::PromiseState::Fulfilled, source)?;
            self.perform_promise_then_with_capability(awaited, None, None, generator_value)?;
            return self.complete_async_await_resolution();
        }
        let awaited = self.create_promise(
            crate::promise_state::PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.perform_promise_then_with_capability(awaited, None, None, generator_value)?;
        self.begin_promise_resolution(
            awaited,
            source,
            NativeContinuationSite {
                caller_base: site.base,
                destination: site.destination,
                call_site: site.instruction,
            },
            crate::runtime::fiber::PromiseResolutionMode::AsyncAwait,
        )
    }

    /// Records one async-generator await destination before observable Promise resolution begins.
    fn prepare_async_generator_await(
        &mut self,
        generator: GcRef<GeneratorObject>,
        destination: u32,
        instruction: WordOffset,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if !generator.is_async
                    || generator.state != GeneratorState::Executing
                    || generator.paused.is_some()
                    || generator.resume_destination.is_some()
                    || generator.resume_kind_destination.is_some()
                    || generator.resume_instruction.is_some()
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                generator.resume_destination = Some(destination);
                generator.resume_instruction = Some(instruction);
                Ok(())
            })
        })
    }

    /// Publishes the awaiting Fiber and restores the Promise-checkpoint caller.
    pub(crate) fn complete_async_generator_await_resolution(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<(), ExecutionError> {
        let paused = core::mem::take(&mut self.fiber);
        let mut paused = Some(paused);
        let caller = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if generator.state != GeneratorState::Executing
                    || generator.paused.is_some()
                    || generator.resume_destination.is_none()
                    || generator.resume_kind_destination.is_some()
                    || generator.resume_instruction.is_none()
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                generator.paused = paused.take();
                generator
                    .caller
                    .take()
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        });
        match caller {
            Ok(caller) => {
                self.fiber = caller;
                Ok(())
            }
            Err(error) => {
                self.fiber = paused.expect("failed async-generator await retains Fiber ownership");
                Err(error)
            }
        }
    }

    /// Reports whether a Promise reaction capability names a currently awaiting async generator.
    pub(crate) fn is_async_generator_await(&mut self, value: Value) -> bool {
        let Ok(generator) = self.generator_reference(value) else {
            return false;
        };
        self.heap.with_running_scope(|scope| {
            let Ok(generator) = scope.root(generator) else {
                return false;
            };
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(generator, self.types.generator_object)
                    .is_ok_and(|generator| {
                        generator.is_async
                            && (generator.state == GeneratorState::AwaitingReturn
                                || (generator.state == GeneratorState::Executing
                                    && generator.paused.is_some()
                                    && generator.resume_destination.is_some()
                                    && generator.resume_instruction.is_some()))
                    })
            })
        })
    }

    /// Reports the constructor-lookup branch used by AsyncGeneratorAwaitReturn.
    pub(crate) fn is_async_generator_awaiting_return(&mut self, value: Value) -> bool {
        let Ok(generator) = self.generator_reference(value) else {
            return false;
        };
        self.generator_header(generator)
            .is_ok_and(|header| header.is_async && header.state == GeneratorState::AwaitingReturn)
    }

    /// Resumes an awaited async generator from a Promise reaction without changing its request head.
    pub(crate) fn resume_async_generator_await_job(
        &mut self,
        generator_value: Value,
        argument: Value,
        rejected: bool,
        return_site: WordOffset,
    ) -> Result<Option<crate::RunOutcome>, ExecutionError> {
        let generator = self.generator_reference(generator_value)?;
        let request = self.active_async_generator_request(generator)?;
        self.fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?
            .pc = return_site;
        if self.generator_header(generator)?.state == GeneratorState::AwaitingReturn {
            self.set_generator_state(generator, GeneratorState::Completed)?;
            let result = if rejected {
                argument
            } else {
                self.create_iterator_result(argument, true)?
            };
            self.settle_active_async_generator_request(generator, result, rejected)?;
            self.promise_jobs.finish_active();
            if self.has_queued_async_generator_request(generator)? {
                self.resume_next_async_generator_request(generator_value)?;
            }
            return Ok(None);
        }
        let continuation = NativeContinuation::async_generator_resume(
            NativeContinuationSite {
                caller_base: self
                    .fiber
                    .frames
                    .last()
                    .ok_or(ExecutionError::MissingEnvironment)?
                    .base,
                destination: 0,
                call_site: return_site,
            },
            generator_value,
            request.promise,
        );
        let caller = core::mem::take(&mut self.fiber);
        let resume = if rejected {
            GeneratorResume::Abrupt(CompletionRecord::throw(argument))
        } else {
            GeneratorResume::Next(argument)
        };
        let prepared =
            match self.swap_generator_caller_for_paused(generator, caller, continuation, resume) {
                Ok(prepared) => prepared,
                Err(rollback) => {
                    self.fiber = rollback.fiber;
                    return Err(rollback.error);
                }
            };
        self.fiber = prepared.fiber;
        self.promise_jobs.finish_active();
        if let Some(abrupt) = prepared.abrupt {
            return self.dispatch_abrupt(abrupt.completion, abrupt.instruction);
        }
        Ok(None)
    }

    /// Suspends the active generator fiber and publishes one observable `{ value, done: false }`.
    pub(crate) fn suspend_generator_yield(
        &mut self,
        site: GeneratorSuspendSite,
    ) -> Result<(), ExecutionError> {
        let frame = self
            .fiber
            .frames
            .last()
            .copied()
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        let continuation_index = frame
            .completion_base
            .checked_sub(1)
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?
            as usize;
        let mut continuation = self
            .fiber
            .completions
            .native_at(continuation_index)
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        let mut generator = self.generator_reference(continuation.first())?;
        let value = self.read(site.base, site.source)?;
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
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        if point.instruction != site.instruction
            || point.destination.index() != site.destination
            || point.resume_offset != frame.pc
            || site
                .kind_destination
                .is_some_and(|kind| kind != site.destination.saturating_add(1))
        {
            return Err(ExecutionError::UnsupportedGeneratorYieldResume);
        }
        let async_result = if continuation.second().as_immediate() != Some(Immediate::Undefined) {
            let result = self.create_iterator_result(value, false)?;
            continuation = self
                .fiber
                .completions
                .native_at(continuation_index)
                .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
            generator = self.generator_reference(continuation.first())?;
            Some(result)
        } else {
            None
        };
        let paused = core::mem::take(&mut self.fiber);
        let caller = match self.set_generator_paused(
            generator,
            paused,
            site.destination,
            site.kind_destination,
            site.instruction,
        ) {
            Ok(caller) => caller,
            Err(rollback) => {
                self.fiber = rollback.fiber;
                return Err(rollback.error);
            }
        };
        self.fiber = caller;
        if let Some(result) = async_result {
            return self.schedule_async_generator_settlement(generator, result, false);
        }
        let result = if site.kind_destination.is_some() {
            value
        } else {
            self.create_iterator_result(value, false)?
        };
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            result,
        )
    }

    /// Takes a paused activation without copying its register, handler, or completion storage.
    #[allow(
        clippy::result_large_err,
        reason = "the allocation-free rollback path must return full Fiber ownership"
    )]
    fn swap_generator_caller_for_paused(
        &mut self,
        generator: GcRef<GeneratorObject>,
        caller: Fiber,
        continuation: NativeContinuation,
        resume: GeneratorResume,
    ) -> Result<PreparedGeneratorResume, FiberTransferError> {
        let mut caller = Some(caller);
        let result = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if generator.caller.is_some() {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                let destination = generator
                    .resume_destination
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
                let instruction = generator
                    .resume_instruction
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
                let kind_destination = generator.resume_kind_destination;
                let paused = generator
                    .paused
                    .as_mut()
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
                let frame = paused
                    .frames
                    .last()
                    .copied()
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
                let continuation_index = frame
                    .completion_base
                    .checked_sub(1)
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?
                    as usize;
                let destination_index = frame.base as usize + destination as usize;
                let kind_destination_index =
                    kind_destination.map(|kind| frame.base as usize + kind as usize);
                if destination_index >= paused.registers.len()
                    || kind_destination_index.is_some_and(|index| index >= paused.registers.len())
                {
                    return Err(ExecutionError::InvalidRegister(
                        tachyon_bytecode::RegisterId::new(destination),
                    ));
                }
                if !paused
                    .completions
                    .replace_native(continuation_index, continuation)
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                let abrupt = match (resume, kind_destination_index) {
                    (GeneratorResume::Next(value), None) => {
                        paused.registers[destination_index] = value;
                        None
                    }
                    (GeneratorResume::Abrupt(completion), None) => Some(InjectedGeneratorAbrupt {
                        completion,
                        instruction,
                    }),
                    (GeneratorResume::Next(value), Some(kind)) => {
                        paused.registers[destination_index] = value;
                        paused.registers[kind] = Value::from_i32(0);
                        None
                    }
                    (GeneratorResume::Abrupt(completion), Some(kind)) => {
                        let value = completion
                            .value()
                            .ok_or(ExecutionError::MissingCompletionRecord)?;
                        let resume_kind = match completion.kind() {
                            CompletionKind::Return => 1,
                            CompletionKind::Throw => 2,
                            _ => return Err(ExecutionError::MissingCompletionRecord),
                        };
                        paused.registers[destination_index] = value;
                        paused.registers[kind] = Value::from_i32(resume_kind);
                        None
                    }
                };
                generator.state = GeneratorState::Executing;
                generator.caller = caller.take();
                generator.resume_destination = None;
                generator.resume_kind_destination = None;
                generator.resume_instruction = None;
                Ok(PreparedGeneratorResume {
                    fiber: generator
                        .paused
                        .take()
                        .expect("validated paused generator fiber remains present"),
                    abrupt,
                })
            })
        });
        result.map_err(|error| FiberTransferError {
            error,
            fiber: caller.expect("failed generator resume retains caller fiber ownership"),
        })
    }

    /// Publishes a complete paused fiber and its resume destination as one generator state change.
    #[allow(
        clippy::result_large_err,
        reason = "the allocation-free rollback path must return full Fiber ownership"
    )]
    fn set_generator_paused(
        &mut self,
        generator: GcRef<GeneratorObject>,
        paused: Fiber,
        destination: u32,
        kind_destination: Option<u32>,
        instruction: WordOffset,
    ) -> Result<Fiber, FiberTransferError> {
        let mut paused = Some(paused);
        let result = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if generator.caller.is_none()
                    || generator.paused.is_some()
                    || generator.resume_destination.is_some()
                    || generator.resume_kind_destination.is_some()
                    || generator.resume_instruction.is_some()
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                let caller = generator
                    .caller
                    .take()
                    .expect("validated generator caller remains present");
                generator.paused = paused.take();
                generator.resume_destination = Some(destination);
                generator.resume_kind_destination = kind_destination;
                generator.resume_instruction = Some(instruction);
                generator.state = GeneratorState::SuspendedYield;
                Ok(caller)
            })
        });
        result.map_err(|error| FiberTransferError {
            error,
            fiber: paused.expect("failed paused publication retains fiber ownership"),
        })
    }

    /// Restores a paused generator fiber and injects the next(value) result at its yield destination.
    fn resume_generator_yield(
        &mut self,
        generator: GcRef<GeneratorObject>,
        site: &CallSite,
        resume_value: Option<Value>,
    ) -> Result<(), ExecutionError> {
        let resume_value = match resume_value {
            Some(value) => value,
            None => self
                .call_argument(site, 0)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined)),
        };
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let continuation = if self.generator_header(generator)?.is_async {
            let request = self.active_async_generator_request(generator)?;
            NativeContinuation::async_generator_resume(
                continuation_site,
                site.this_value,
                request.promise,
            )
        } else {
            NativeContinuation::generator_resume(continuation_site, site.this_value)
        };
        let caller = core::mem::take(&mut self.fiber);
        match self.swap_generator_caller_for_paused(
            generator,
            caller,
            continuation,
            GeneratorResume::Next(resume_value),
        ) {
            Ok(prepared) => {
                debug_assert!(prepared.abrupt.is_none());
                self.fiber = prepared.fiber;
                Ok(())
            }
            Err(rollback) => {
                self.fiber = rollback.fiber;
                Err(rollback.error)
            }
        }
    }

    /// Restores one paused Fiber and routes injected Return/Throw through its protected ranges.
    fn resume_generator_abrupt(
        &mut self,
        generator: GcRef<GeneratorObject>,
        site: &CallSite,
        completion: CompletionRecord,
        is_async: bool,
    ) -> Result<(), ExecutionError> {
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let continuation = if is_async {
            let request = self.active_async_generator_request(generator)?;
            NativeContinuation::async_generator_resume(
                continuation_site,
                site.this_value,
                request.promise,
            )
        } else {
            NativeContinuation::generator_resume(continuation_site, site.this_value)
        };
        let caller = core::mem::take(&mut self.fiber);
        let prepared = match self.swap_generator_caller_for_paused(
            generator,
            caller,
            continuation,
            GeneratorResume::Abrupt(completion),
        ) {
            Ok(prepared) => prepared,
            Err(rollback) => {
                self.fiber = rollback.fiber;
                return Err(rollback.error);
            }
        };
        self.fiber = prepared.fiber;
        let Some(abrupt) = prepared.abrupt else {
            return Ok(());
        };
        match self.dispatch_abrupt(abrupt.completion, abrupt.instruction) {
            Ok(None) => Ok(()),
            Ok(Some(crate::RunOutcome::Thrown(value))) => Err(ExecutionError::HostThrown(value)),
            Ok(Some(_)) => Err(ExecutionError::MissingCompletionRecord),
            Err(error) => {
                self.abort_generator_execution(generator)?;
                Err(error)
            }
        }
    }

    /// Restores the caller and closes a generator after an engine fault during abrupt routing.
    fn abort_generator_execution(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<(), ExecutionError> {
        self.set_generator_state(generator, GeneratorState::Completed)?;
        self.restore_generator_caller_fiber(generator)
    }

    /// Releases the creation-time roots after their ownership is visible through the frame.
    fn clear_generator_activation(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .activation = None;
                Ok(())
            })
        })
    }

    /// Reports the exact immutable argument backing retained by a generator in VM tests.
    #[cfg(test)]
    pub(crate) fn generator_retained_argument_capacity(
        &mut self,
        value: Value,
    ) -> Result<usize, ExecutionError> {
        let generator = self.generator_reference(value)?;
        let argument_prefix = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(generator
                    .activation
                    .map(|activation| activation.argument_prefix))
            })
        })?;
        let Some(argument_prefix) = argument_prefix else {
            return Ok(0);
        };
        self.heap.with_running_scope(|scope| {
            let argument_prefix = scope.root(argument_prefix).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(argument_prefix, self.types.bound_function)
                    .map(|prefix| prefix.arguments.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_generator_state(
        &mut self,
        generator: GcRef<GeneratorObject>,
        state: GeneratorState,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .state = state;
                Ok(())
            })
        })
    }
}
