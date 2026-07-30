//! Generator instance state and the retained call payload used by first resume.

use std::collections::VecDeque;

use tachyon_gc::{GcRef, Trace, Tracer};
use tachyon_value::Value;

use crate::{
    AllocationSpace, BoundFunctionData, ExecutionError, FunctionKind, Immediate, Isolate,
    NativeErrorKind, ShapeId,
    object::OrdinaryObject,
    runtime::{
        callable::{CallSite, ResolvedCallTarget},
        code::CodeId,
        completion::{CompletionKind, CompletionRecord},
        fiber::{Fiber, NativeContinuation, NativeContinuationSite, VmRoots},
    },
};
use tachyon_bytecode::WordOffset;

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

/// GC-managed generator instance owning its paused bytecode Fiber.
#[derive(Debug)]
pub(crate) struct GeneratorObject {
    pub(crate) ordinary: OrdinaryObject,
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
    /// Creates one unpublished generator used while its parameter prologue runs.
    fn new(ordinary: OrdinaryObject, is_async: bool) -> Self {
        Self {
            ordinary,
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
            state: self.state,
            is_async: self.is_async,
        }
    }
}

impl Trace for GeneratorObject {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
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
    state: GeneratorState,
    is_async: bool,
}

struct GeneratorInitializationRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    callee: Value,
    this_value: Value,
    argument_prefix: GcRef<BoundFunctionData>,
}

impl Trace for GeneratorInitializationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.callee.trace(tracer);
        self.this_value.trace(tracer);
        self.argument_prefix.trace(tracer);
    }
}

impl Isolate {
    /// Starts a generator synchronously so parameter initialization precedes object publication.
    pub(crate) fn begin_generator_initialization(
        &mut self,
        site: &CallSite,
        target: ResolvedCallTarget,
    ) -> Result<(), ExecutionError> {
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
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(site.callee, prototype_atom)?
            .filter(|prototype| self.is_object_value(*prototype))
            .unwrap_or_else(|| {
                if target.kind == FunctionKind::AsyncGenerator {
                    self.realm
                        .async_generator_prototype
                        .expect("async generator intrinsics initialize before generator calls")
                } else {
                    self.realm
                        .generator_prototype
                        .expect("generator intrinsics initialize before generator calls")
                }
            });
        self.write(site.caller_base, site.destination, prototype)?;
        let argument_prefix =
            self.create_apply_argument_prefix(site.callee, this_value, arguments)?;
        let prototype = self.read(site.caller_base, site.destination)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(argument_prefix.raw()),
        )?;
        let prefix = self.bound_function_snapshot(argument_prefix)?;
        let mut roots = GeneratorInitializationRoots {
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
            prototype,
            callee: prefix.call_target,
            this_value: prefix.bound_this,
            argument_prefix,
        };
        let generator = self
            .heap
            .try_allocate_with_gc(
                self.types.generator_object,
                0,
                0,
                GeneratorObject::new(
                    OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    target.kind == FunctionKind::AsyncGenerator,
                ),
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let callee = roots.callee;
        let this_value = roots.this_value;
        let argument_prefix = roots.argument_prefix;
        let generator_value = Value::from_heap_ref(generator.raw());
        self.write(site.caller_base, site.destination, generator_value)?;
        let continuation = NativeContinuation::generator_initialize(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            generator_value,
            callee,
        );
        let caller = core::mem::take(&mut self.fiber);
        if let Err(rollback) = self.set_generator_caller(generator, caller) {
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
                    target,
                    CallSite {
                        caller_base: site.caller_base,
                        destination: site.destination,
                        callee,
                        argument_base: 0,
                        argument_source: None,
                        argument_prefix: Some(argument_prefix),
                        argument_prefix_offset: 0,
                        argument_prefix_count: site.argument_count,
                        argument_count: site.argument_count,
                        this_value,
                        new_target: Value::from_immediate(Immediate::Undefined),
                        construct_receiver: None,
                        call_site: site.call_site,
                    },
                )
            });
        if let Err(error) = pushed {
            let _ = self.fiber.completions.pop_native();
            self.set_generator_state(generator, GeneratorState::Completed)?;
            self.restore_generator_caller_fiber(generator)?;
            return Err(error);
        }
        self.fiber
            .frames
            .last_mut()
            .expect("generator initialization publishes one bytecode frame")
            .return_continuation = true;
        Ok(())
    }

    /// Starts a suspended generator on the existing iterative frame machine.
    pub(crate) fn begin_generator_next(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_generator_next_kind(site, false, None)
    }

    /// Publishes a generator only after its parameter and declaration prologue has completed.
    pub(crate) fn suspend_generator_initialization(&mut self) -> Result<(), ExecutionError> {
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
                    == crate::runtime::fiber::NativeContinuationKind::GeneratorInitialize
            })
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        let generator = self.generator_reference(continuation.first())?;
        let prototype_atom = self.prototype_atom()?;
        let function_prototype = self
            .get_data_property(continuation.second(), prototype_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let prototype = if self.is_object_value(function_prototype) {
            function_prototype
        } else if self.generator_header(generator)?.is_async {
            self.realm
                .async_generator_prototype
                .expect("async generator intrinsics initialize before generator calls")
        } else {
            self.realm
                .generator_prototype
                .expect("generator intrinsics initialize before generator calls")
        };
        self.set_generator_prototype(generator, prototype)?;
        let continuation = self
            .fiber
            .completions
            .native_at(continuation_index)
            .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)?;
        let generator = self.generator_reference(continuation.first())?;
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
                    || generator.resume_destination.is_some()
                    || generator.resume_instruction.is_some()
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                generator.paused = paused.take();
                generator.state = GeneratorState::SuspendedStart;
                generator
                    .caller
                    .take()
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        });
        let caller = match caller {
            Ok(caller) => caller,
            Err(error) => {
                self.fiber = paused.expect("failed initialization pause retains Fiber ownership");
                return Err(error);
            }
        };
        self.fiber = caller;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            continuation.first(),
        )
    }

    /// Restores the already-initialized Fiber; the first next argument is intentionally ignored.
    fn resume_initialized_generator(
        &mut self,
        generator: GcRef<GeneratorObject>,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let header = self.generator_header(generator)?;
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
        let caller = core::mem::take(&mut self.fiber);
        let mut caller = Some(caller);
        let resumed = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if generator.state != GeneratorState::SuspendedStart
                    || generator.caller.is_some()
                    || generator.resume_destination.is_some()
                    || generator.resume_instruction.is_some()
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
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
                if !paused
                    .completions
                    .replace_native(continuation_index, continuation)
                {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                generator.caller = caller.take();
                generator.state = GeneratorState::Executing;
                generator
                    .paused
                    .take()
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        });
        match resumed {
            Ok(resumed) => {
                self.fiber = resumed;
                Ok(())
            }
            Err(error) => {
                self.fiber = caller.expect("failed initialized resume retains caller Fiber");
                Err(error)
            }
        }
    }

    /// Restores the synchronous caller when parameter initialization throws.
    pub(crate) fn finish_generator_initialization_throw(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(continuation.first())?;
        self.set_generator_state(generator, GeneratorState::Completed)?;
        self.restore_generator_caller_fiber(generator)
    }

    /// Replaces the unpublished generator's prototype and records the managed edge.
    fn set_generator_prototype(
        &mut self,
        generator: GcRef<GeneratorObject>,
        prototype: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .ordinary
                    .prototype = prototype;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(generator, prototype)
                .map(|_| ())
                .map_err(ExecutionError::HeapReference)
        })
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
                self.write(site.caller_base, site.destination, result)
            }
            GeneratorState::Executing | GeneratorState::AwaitingReturn => {
                Err(ExecutionError::GeneratorExecuting)
            }
            GeneratorState::SuspendedYield => {
                self.resume_generator_yield(generator, site, resume_value)
            }
            GeneratorState::SuspendedStart => self.resume_initialized_generator(generator, site),
        }
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
            return self.settle_and_resume_async_generator_request(generator, result, false);
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
        self.settle_and_resume_async_generator_request(generator, reason, true)?;
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

    /// Resolves the active request in the current job and immediately consumes the FIFO head.
    fn settle_and_resume_async_generator_request(
        &mut self,
        generator: GcRef<GeneratorObject>,
        result: Value,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        self.settle_active_async_generator_request(generator, result, rejected)?;
        if self.has_queued_async_generator_request(generator)? {
            self.resume_next_async_generator_request(Value::from_heap_ref(generator.raw()))?;
        }
        Ok(())
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
                if generator.state != GeneratorState::SuspendedStart || generator.caller.is_some() {
                    return Err(ExecutionError::UnsupportedGeneratorYieldResume);
                }
                generator.paused = None;
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
            return self.settle_and_resume_async_generator_request(generator, result, false);
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

    /// Reports the exact immutable argument backing retained by a paused generator in VM tests.
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
                    .paused
                    .as_ref()
                    .and_then(|fiber| fiber.frames.last().and_then(|frame| frame.argument_prefix)))
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
