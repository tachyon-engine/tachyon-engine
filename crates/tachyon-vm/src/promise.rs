//! Promise state, reaction records, and the isolate-owned FIFO microtask substrate.

use super::*;

const FIN_SOURCE: usize = 0;
const FIN_CALLBACK: usize = 1;
const FIN_FULFILLED: usize = 2;
const FIN_REJECTED: usize = 3;
const FIN_CONSTRUCTOR: usize = 4;
const FIN_RESULT_ORIGINAL: usize = 0;
const FIN_RESULT_REJECTED: usize = 2;
const CONSTRUCTOR_EXECUTOR: usize = 0;
const CONSTRUCTOR_NEW_TARGET: usize = 1;
const CONSTRUCTOR_PROTOTYPE: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseCheckpointProgress {
    Progressed,
    Suspended,
    Completed(RunOutcome),
}

struct PromiseConstructorRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for PromiseConstructorRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Allocates one Promise with its state/result initialized before publication.
    pub(crate) fn create_promise(
        &mut self,
        state: PromiseState,
        result: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .promise_prototype
            .expect("Promise prototype initializes before Promise allocation");
        self.create_promise_with_prototype(state, result, prototype)
    }

    /// Allocates a Promise with the prototype selected from the active constructor/newTarget.
    pub(crate) fn create_promise_with_prototype(
        &mut self,
        state: PromiseState,
        result: Value,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
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
            .try_allocate_with_gc(
                self.types.promise_object,
                0,
                0,
                PromiseObject {
                    state,
                    result,
                    fulfill_head: None,
                    fulfill_tail: None,
                    reject_head: None,
                    reject_tail: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|promise| Value::from_heap_ref(promise.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Copies Promise state without retaining a heap borrow across an allocation.
    pub(crate) fn promise_snapshot(
        &mut self,
        value: Value,
    ) -> Result<PromiseObject, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let promise = self
            .heap
            .checked_reference(raw, self.types.promise_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let promise = scope.root(promise).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(promise, self.types.promise_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Creates the shared one-shot cell and the two strict native resolving callables.
    pub(crate) fn create_promise_capability_arguments(
        &mut self,
        promise: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = PromiseCapabilityRoots {
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
            promise,
            cell: None,
            resolve: undefined,
            reject: undefined,
        };
        let cell = self
            .heap
            .try_allocate_with_gc(
                self.types.promise_resolution_cell,
                0,
                0,
                PromiseResolutionCell {
                    promise: roots.promise,
                    already_resolved: false,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.cell = Some(cell);
        let prototype = roots
            .vm
            .realm
            .function_prototype
            .expect("Function prototype initializes before Promise resolvers");
        roots.resolve = allocate_promise_resolver(
            &mut self.heap,
            self.types.function,
            cell,
            false,
            prototype,
            &mut roots,
        )?;
        roots.reject = allocate_promise_resolver(
            &mut self.heap,
            self.types.function,
            cell,
            true,
            prototype,
            &mut roots,
        )?;
        let values = [
            roots.resolve,
            roots.reject,
            roots.promise,
            undefined,
            undefined,
        ];
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                NativeCallState { values, count: 2 },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Calls the executor through the VM trampoline and retains the result Promise in its continuation.
    pub(crate) fn begin_promise_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let executor = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(executor)?;
        let prototype_atom = self.prototype_atom()?;
        match self.resolve_property_read_until_proxy(site.new_target, prototype_atom.into())? {
            PropertyReadResolution::Read(PropertyRead::Missing) => self.finish_promise_constructor(
                site,
                executor,
                site.new_target,
                Value::from_immediate(Immediate::Undefined),
                None,
            ),
            PropertyReadResolution::Read(PropertyRead::Data(prototype)) => {
                self.finish_promise_constructor(site, executor, site.new_target, prototype, None)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.finish_promise_constructor(
                    site,
                    executor,
                    site.new_target,
                    Value::from_immediate(Immediate::Undefined),
                    None,
                )
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => {
                self.dispatch_promise_constructor_prototype(site, executor, Some(getter))
            }
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_promise_constructor_prototype(site, executor, None)
            }
        }
    }

    /// Publishes constructor inputs before the observable prototype Get can suspend or collect.
    fn dispatch_promise_constructor_prototype(
        &mut self,
        site: &CallSite,
        executor: Value,
        getter: Option<Value>,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_promise_constructor_state(NativeCallState {
            values: [executor, site.new_target, undefined, undefined, undefined],
            count: 0,
        })?;
        let state_value = Value::from_heap_ref(state.raw());
        self.write(site.caller_base, site.destination, state_value)?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let continuation = NativeContinuation::promise_constructor_prototype(
            native_site,
            state_value,
            site.new_target,
        );
        if let Some(getter) = getter {
            self.dispatch_property_callback(continuation, getter)?;
            return Ok(());
        }

        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Isolate::completion_stack_error)?;
        let prototype_atom = self.prototype_atom()?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(
            native_site,
            site.new_target,
            site.new_target,
            prototype_atom.into(),
        ) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let prototype = self.read(site.caller_base, site.destination)?;
        self.resume_promise_constructor(continuation, prototype)
    }

    /// Restores the managed state root, captures the prototype, and enters Promise allocation.
    pub(crate) fn resume_promise_constructor(
        &mut self,
        continuation: NativeContinuation,
        prototype: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            continuation.first(),
        )?;
        self.set_promise_constructor_value(state, CONSTRUCTOR_PROTOTYPE, prototype)?;
        let pending = self.native_call_state_snapshot(state)?;
        let site = CallSite {
            caller_base: continuation.site().caller_base,
            destination: continuation.site().destination,
            callee: Value::from_immediate(Immediate::Undefined),
            argument_base: 0,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: pending.values[CONSTRUCTOR_NEW_TARGET],
            construct_receiver: None,
            call_site: continuation.site().call_site,
        };
        self.finish_promise_constructor(
            &site,
            pending.values[CONSTRUCTOR_EXECUTOR],
            pending.values[CONSTRUCTOR_NEW_TARGET],
            pending.values[CONSTRUCTOR_PROTOTYPE],
            Some(state),
        )
    }

    /// Allocates the Promise and calls its executor after prototype selection has completed.
    fn finish_promise_constructor(
        &mut self,
        site: &CallSite,
        executor: Value,
        new_target: Value,
        candidate_prototype: Value,
        state: Option<GcRef<NativeCallState>>,
    ) -> Result<(), ExecutionError> {
        let prototype = if self.is_object_value(candidate_prototype) {
            candidate_prototype
        } else {
            self.realm_for_callable(new_target)
                .ok()
                .and_then(|realm| {
                    self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::Promise)
                })
                .ok_or(ExecutionError::MissingNativeContinuation)?
        };
        let promise = self.create_promise_with_prototype(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
            prototype,
        )?;
        if let Some(state) = state {
            self.set_promise_constructor_value(state, CONSTRUCTOR_PROTOTYPE, promise)?;
        } else {
            self.write(site.caller_base, site.destination, promise)?;
        }
        let arguments = self.create_promise_capability_arguments(promise)?;
        if state.is_some() {
            self.write(site.caller_base, site.destination, promise)?;
        }
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_executor(
                continuation_site,
                promise,
                Value::from_heap_ref(arguments.raw()),
            ))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: executor,
            argument_base: 0,
            argument_source: Some(arguments),
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 2,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            if let Some(kind) = execution_error_kind(&error) {
                let thrown = self.create_native_error(kind, None)?;
                self.settle_promise(promise, PromiseState::Rejected, thrown)?;
                return self.write(site.caller_base, site.destination, promise);
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Promise executor bytecode call publishes a frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        debug_assert_eq!(continuation.kind(), NativeContinuationKind::PromiseExecutor);
        self.write(site.caller_base, site.destination, promise)
    }

    /// Allocates the fixed constructor state under the complete VM root set.
    fn allocate_promise_constructor_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = PromiseConstructorRoots {
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
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Stores one constructor edge and applies the old-to-young write barrier.
    fn set_promise_constructor_value(
        &mut self,
        state: GcRef<NativeCallState>,
        index: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[index] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Claims one shared resolving-function cell before any observable resolution work.
    pub(crate) fn claim_promise_resolver(
        &mut self,
        cell: GcRef<PromiseResolutionCell>,
    ) -> Result<Option<Value>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let cell = scope.root(cell).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let cell = no_gc
                    .borrow_mut(cell, self.types.promise_resolution_cell)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if cell.already_resolved {
                    return Ok(None);
                }
                cell.already_resolved = true;
                Ok(Some(cell.promise))
            })
        })
    }

    /// Starts one resolving-function call without recursively entering the interpreter.
    pub(crate) fn begin_promise_resolver_call(
        &mut self,
        site: &CallSite,
        cell: GcRef<PromiseResolutionCell>,
        reject: bool,
        resolution: Value,
    ) -> Result<(), ExecutionError> {
        let promise = self.claim_promise_resolver(cell)?;
        let Some(promise) = promise else {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        };
        if reject {
            self.settle_promise(promise, PromiseState::Rejected, resolution)?;
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        self.begin_promise_resolution(
            promise,
            resolution,
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            PromiseResolutionMode::ResolverCall,
        )
    }

    /// Runs the Promise Resolution Procedure through an observable, resumable `then` lookup.
    pub(crate) fn begin_promise_resolution(
        &mut self,
        promise: Value,
        resolution: Value,
        site: NativeContinuationSite,
        mode: PromiseResolutionMode,
    ) -> Result<(), ExecutionError> {
        if promise == resolution {
            let error = self.create_native_error(NativeErrorKind::Type, None)?;
            self.settle_promise(promise, PromiseState::Rejected, error)?;
            return self.complete_promise_resolution(site, mode, promise);
        }
        if !self.is_object_value(resolution) {
            self.settle_promise(promise, PromiseState::Fulfilled, resolution)?;
            return self.complete_promise_resolution(site, mode, promise);
        }

        let continuation = NativeContinuation::promise_resolution(site, mode, promise, resolution);
        let completion_base = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Isolate::completion_stack_error)?;
        let then_atom = match self.intern_intrinsic_name(b"then") {
            Ok(atom) => atom,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let read = match self.resolve_property_read_until_proxy(resolution, then_atom.into()) {
            Ok(read) => read,
            Err(error) => {
                let Some(kind) = execution_error_kind(&error) else {
                    self.pop_native_continuation()?;
                    return Err(error);
                };
                let reason = self.create_native_error(kind, None)?;
                self.pop_native_continuation()?;
                return self.reject_promise_resolution(continuation, mode, reason);
            }
        };
        match read {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.pop_native_continuation()?;
                self.finish_promise_resolution(
                    continuation,
                    mode,
                    Value::from_immediate(Immediate::Undefined),
                )
            }
            PropertyReadResolution::Read(PropertyRead::Data(then)) => {
                self.pop_native_continuation()?;
                self.finish_promise_resolution(continuation, mode, then)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.pop_native_continuation()?;
                self.finish_promise_resolution(
                    continuation,
                    mode,
                    Value::from_immediate(Immediate::Undefined),
                )
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => {
                self.pop_native_continuation()?;
                match self.dispatch_property_callback(continuation, getter) {
                    Ok(_) => Ok(()),
                    Err(error) => self.reject_promise_resolution_error(continuation, mode, error),
                }
            }
            PropertyReadResolution::Proxy(_) => {
                let frame_depth = self.fiber.frames.len();
                let dispatched = self.dispatch_proxy_aware_property_read(
                    site,
                    resolution,
                    resolution,
                    then_atom.into(),
                );
                if let Err(error) = dispatched {
                    let Some(kind) = execution_error_kind(&error) else {
                        if self.fiber.completions.len() > completion_base {
                            self.pop_native_continuation()?;
                        }
                        return Err(error);
                    };
                    let reason = self.create_native_error(kind, None)?;
                    if self.fiber.completions.len() > completion_base {
                        self.pop_native_continuation()?;
                    }
                    return self.reject_promise_resolution(continuation, mode, reason);
                }
                if self.fiber.frames.len() != frame_depth
                    || self.fiber.completions.len() == completion_base
                {
                    return Ok(());
                }
                let continuation = self.pop_native_continuation()?;
                let then = self.read(site.caller_base, site.destination)?;
                self.finish_promise_resolution(continuation, mode, then)
            }
        }
    }

    /// Converts an observable `then` lookup failure into rejection at the Promise boundary.
    fn reject_promise_resolution_error(
        &mut self,
        continuation: NativeContinuation,
        mode: PromiseResolutionMode,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let Some(kind) = execution_error_kind(&error) else {
            return Err(error);
        };
        let reason = self.create_native_error(kind, None)?;
        self.reject_promise_resolution(continuation, mode, reason)
    }

    /// Completes resolution after `then` lookup and enqueues callable thenables as jobs.
    pub(crate) fn finish_promise_resolution(
        &mut self,
        continuation: NativeContinuation,
        mode: PromiseResolutionMode,
        then: Value,
    ) -> Result<(), ExecutionError> {
        let promise = continuation.first();
        let resolution = continuation.second();
        if self.resolve_function_object(then).is_ok() {
            self.promise_jobs.push(PromiseJob::Thenable {
                promise,
                thenable: resolution,
                then,
            });
        } else {
            self.settle_promise(promise, PromiseState::Fulfilled, resolution)?;
        }
        self.complete_promise_resolution(continuation.site(), mode, promise)
    }

    /// Rejects a pending resolution while preserving the caller-specific return contract.
    pub(crate) fn reject_promise_resolution(
        &mut self,
        continuation: NativeContinuation,
        mode: PromiseResolutionMode,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let promise = continuation.first();
        self.settle_promise(promise, PromiseState::Rejected, reason)?;
        self.complete_promise_resolution(continuation.site(), mode, promise)
    }

    /// Restores the native caller or active reaction after resolution reaches a stable state.
    fn complete_promise_resolution(
        &mut self,
        site: NativeContinuationSite,
        mode: PromiseResolutionMode,
        promise: Value,
    ) -> Result<(), ExecutionError> {
        match mode {
            PromiseResolutionMode::ResolverCall => self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            ),
            PromiseResolutionMode::StaticResolve => {
                self.write(site.caller_base, site.destination, promise)
            }
            PromiseResolutionMode::Internal => Ok(()),
            PromiseResolutionMode::Reaction => {
                self.promise_jobs.finish_active();
                if let Some(frame) = self.fiber.frames.last_mut() {
                    frame.pc = site.call_site;
                }
                Ok(())
            }
            PromiseResolutionMode::AsyncAwait => self.complete_async_await_resolution(),
        }
    }

    /// Transitions a pending Promise exactly once and publishes its result through the GC barrier.
    pub(crate) fn settle_promise(
        &mut self,
        promise: Value,
        state: PromiseState,
        result: Value,
    ) -> Result<(), ExecutionError> {
        debug_assert_ne!(state, PromiseState::Pending);
        let raw = promise
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(promise))?;
        let promise = self
            .heap
            .checked_reference(raw, self.types.promise_object)
            .map_err(|_| ExecutionError::NotObject(promise))?;
        let reactions = self.heap.with_running_scope(|scope| {
            let promise = scope.root(promise).map_err(ExecutionError::Root)?;
            let reactions = scope.with_no_gc_scope(|no_gc| {
                let promise = no_gc
                    .borrow_mut(promise, self.types.promise_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if promise.state != PromiseState::Pending {
                    return Ok(None);
                }
                promise.state = state;
                promise.result = result;
                let reactions = if state == PromiseState::Rejected {
                    promise.reject_head
                } else {
                    promise.fulfill_head
                };
                promise.fulfill_head = None;
                promise.fulfill_tail = None;
                promise.reject_head = None;
                promise.reject_tail = None;
                Ok(Some(reactions))
            })?;
            if reactions.is_some() {
                scope
                    .write_value_barrier(promise, result)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(reactions.flatten())
        })?;
        self.enqueue_promise_reaction_list(reactions, result, state == PromiseState::Rejected)
    }

    /// Implements the current intrinsic catch path through the shared reaction substrate.
    pub(crate) fn promise_catch(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let on_rejected = self.call_argument(site, 0)?;
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [
                site.this_value,
                on_rejected.unwrap_or(undefined),
                receiver,
                undefined,
                undefined,
            ],
            count: 2,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let then_atom = self.intern_intrinsic_name(b"then")?;
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_catch(
                continuation_site,
                PromiseCatchStage::Then,
                Value::from_heap_ref(state.raw()),
                site.this_value,
            ))
            .map_err(Isolate::completion_stack_error)?;
        let outcome = self.dispatch_proxy_aware_property_read(
            continuation_site,
            receiver,
            site.this_value,
            then_atom.into(),
        );
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        self.pop_native_continuation()?;
        let then = self.read(continuation_site.caller_base, continuation_site.destination)?;
        self.resume_promise_catch(continuation_site, state, PromiseCatchStage::Then, then)
    }

    /// Resumes catch's observable `then` lookup and invokes it with the standard catch arguments.
    pub(crate) fn resume_promise_catch(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: PromiseCatchStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            PromiseCatchStage::Then => {
                if !self.is_callable_value(value)? {
                    return Err(ExecutionError::NonCallable(value));
                }
                let pending = self.native_call_state_snapshot(state)?;
                let undefined = Value::from_immediate(Immediate::Undefined);
                let arguments = self.allocate_promise_then_state(NativeCallState {
                    values: [
                        undefined,
                        pending.values[1],
                        undefined,
                        undefined,
                        undefined,
                    ],
                    count: 2,
                })?;
                let continuation = NativeContinuation::promise_catch(
                    site,
                    PromiseCatchStage::ThenCall,
                    Value::from_heap_ref(state.raw()),
                    value,
                );
                let completion_depth = self.fiber.completions.len();
                let frame_depth = self.fiber.frames.len();
                self.fiber
                    .completions
                    .push_native(continuation)
                    .map_err(Isolate::completion_stack_error)?;
                if let Err(error) = self.call(CallSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    callee: value,
                    argument_base: 0,
                    argument_source: Some(arguments),
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: 2,
                    this_value: pending.values[0],
                    new_target: undefined,
                    construct_receiver: None,
                    call_site: site.call_site,
                }) {
                    self.pop_native_continuation()?;
                    return Err(error);
                }
                if self.fiber.frames.len() != frame_depth {
                    let frame = self
                        .fiber
                        .frames
                        .last_mut()
                        .ok_or(ExecutionError::MissingEnvironment)?;
                    frame.return_register = None;
                    frame.return_continuation = true;
                    return Ok(());
                }
                if self.fiber.completions.len() > completion_depth {
                    self.pop_native_continuation()?;
                }
                Ok(())
            }
            PromiseCatchStage::ThenCall => self.write(site.caller_base, site.destination, value),
        }
    }

    /// Implements the first Promise.prototype.finally slice with traced reaction wrappers.
    ///
    /// The wrapper invokes the user callback and restores the original settlement argument;
    /// callback-returned thenables are handled by the follow-up resolution continuation.
    pub(crate) fn promise_finally(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.this_value) {
            return Err(ExecutionError::NotObject(site.this_value));
        }
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [site.this_value, callback, undefined, undefined, undefined],
            count: 2,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let constructor = self.constructor_atom()?;
        self.dispatch_promise_finally_get(
            continuation_site,
            state,
            PromiseFinallyMethodStage::Constructor,
            site.this_value,
            constructor.into(),
        )
    }

    /// Resumes the observable SpeciesConstructor and `then` lookup for finally.
    pub(crate) fn resume_promise_finally_method(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: PromiseFinallyMethodStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let pending = self.native_call_state_snapshot(state)?;
        match stage {
            PromiseFinallyMethodStage::Constructor => {
                let constructor = if value.as_immediate() == Some(Immediate::Undefined) {
                    self.realm
                        .promise_constructor
                        .expect("Promise initializes before finally")
                } else {
                    if !self.is_object_value(value) {
                        return Err(ExecutionError::NotObject(value));
                    }
                    value
                };
                self.set_promise_then_value(state, FIN_CONSTRUCTOR, constructor)?;
                let species = self
                    .realm
                    .well_known_symbols
                    .species
                    .expect("Symbol.species initializes before Promise");
                let species_key = self.property_key(species)?;
                self.dispatch_promise_finally_get(
                    site,
                    state,
                    PromiseFinallyMethodStage::Species,
                    constructor,
                    species_key,
                )
            }
            PromiseFinallyMethodStage::Species => {
                let intrinsic = self
                    .realm
                    .promise_constructor
                    .expect("Promise initializes before finally");
                let constructor = if matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                ) {
                    intrinsic
                } else {
                    if self.resolve_function_object(value).is_err() {
                        return Err(ExecutionError::NonConstructor(value));
                    }
                    value
                };
                let callback = pending.values[FIN_CALLBACK];
                let callable = self.resolve_function_object(callback).is_ok();
                if callable {
                    let on_fulfilled =
                        self.allocate_promise_finally_handler(callback, constructor, false)?;
                    self.set_promise_then_value(state, FIN_FULFILLED, on_fulfilled)?;
                    let pending = self.native_call_state_snapshot(state)?;
                    let on_rejected = self.allocate_promise_finally_handler(
                        pending.values[FIN_CALLBACK],
                        pending.values[FIN_CONSTRUCTOR],
                        true,
                    )?;
                    self.set_promise_then_value(state, FIN_REJECTED, on_rejected)?;
                } else {
                    self.set_promise_then_value(state, FIN_FULFILLED, callback)?;
                    self.set_promise_then_value(state, FIN_REJECTED, callback)?;
                }
                let then_atom = self.intern_intrinsic_name(b"then")?;
                let pending = self.native_call_state_snapshot(state)?;
                self.dispatch_promise_finally_get(
                    site,
                    state,
                    PromiseFinallyMethodStage::Then,
                    pending.values[FIN_SOURCE],
                    then_atom.into(),
                )
            }
            PromiseFinallyMethodStage::Then => {
                self.resolve_function_object(value)
                    .map_err(|_| ExecutionError::NonCallable(value))?;
                self.set_promise_then_value(state, FIN_CALLBACK, value)?;
                let pending = self.native_call_state_snapshot(state)?;
                let mapping = pending.count == 4;
                let arguments = self.allocate_promise_then_state(NativeCallState {
                    values: [
                        pending.values[FIN_FULFILLED],
                        if mapping {
                            Value::from_immediate(Immediate::Undefined)
                        } else {
                            pending.values[FIN_REJECTED]
                        },
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                    ],
                    count: if mapping { 1 } else { 2 },
                })?;
                let pending = self.native_call_state_snapshot(state)?;
                let then = pending.values[FIN_CALLBACK];
                let continuation = NativeContinuation::promise_finally_method(
                    site,
                    PromiseFinallyMethodStage::ThenCall,
                    Value::from_heap_ref(state.raw()),
                    then,
                );
                let completion_depth = self.fiber.completions.len();
                self.fiber
                    .completions
                    .push_native(continuation)
                    .map_err(Isolate::completion_stack_error)?;
                let frame_depth = self.fiber.frames.len();
                if let Err(error) = self.call(CallSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    callee: then,
                    argument_base: 0,
                    argument_source: Some(arguments),
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: if mapping { 1 } else { 2 },
                    this_value: pending.values[FIN_SOURCE],
                    new_target: Value::from_immediate(Immediate::Undefined),
                    construct_receiver: None,
                    call_site: site.call_site,
                }) {
                    self.pop_native_continuation()?;
                    return Err(error);
                }
                if self.fiber.frames.len() != frame_depth {
                    let frame = self
                        .fiber
                        .frames
                        .last_mut()
                        .expect("finally then call publishes one frame");
                    frame.return_register = None;
                    frame.return_continuation = true;
                } else if self.fiber.completions.len() == completion_depth + 1 {
                    self.pop_native_continuation()?;
                    let result = self.read(site.caller_base, site.destination)?;
                    return self.finish_promise_finally_mapping(site, result);
                } else {
                    return Ok(());
                }
                Ok(())
            }
            PromiseFinallyMethodStage::ThenCall => self.finish_promise_finally_mapping(site, value),
        }
    }

    /// Completes the observable restoration `then` and settles the surrounding reaction.
    fn finish_promise_finally_mapping(
        &mut self,
        site: NativeContinuationSite,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.write(site.caller_base, site.destination, value)?;
        let frame_completion_base = self
            .fiber
            .frames
            .last()
            .map_or(0, |frame| frame.completion_base as usize);
        if self.fiber.completions.len() > frame_completion_base
            && let Some(parent) = self.fiber.completions.last_native()
            && parent.kind() == NativeContinuationKind::PromiseReaction
        {
            let parent = self.pop_native_continuation()?;
            return self.finish_promise_reaction(parent, value);
        }
        Ok(())
    }

    /// Wraps an observable property read with a finally method continuation.
    fn dispatch_promise_finally_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: PromiseFinallyMethodStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_finally_method(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                receiver,
            ))
            .map_err(Isolate::completion_stack_error)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_promise_finally_method(site, state, stage, value)?;
        let _ = continuation;
        Ok(())
    }

    /// Resolves the callback result, then maps its settlement back to the original reaction.
    pub(crate) fn finish_promise_finally_callback(
        &mut self,
        continuation: NativeContinuation,
        callback_result: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let original = continuation.second();
        let function = self.resolve_function_object(continuation.first())?;
        let FunctionExecutable::PromiseFinallyHandler {
            state, rejected, ..
        } = function.executable
        else {
            return Err(ExecutionError::NonCallable(continuation.first()));
        };
        let constructor = self.native_call_state_snapshot(state)?.values[1];
        let intrinsic = self
            .realm
            .promise_constructor
            .expect("Promise initializes before finally");
        if constructor != intrinsic {
            let result_state = self.allocate_promise_then_state(NativeCallState {
                values: [
                    original,
                    constructor,
                    if rejected {
                        Value::from_immediate(Immediate::True)
                    } else {
                        Value::from_immediate(Immediate::False)
                    },
                    Value::from_immediate(Immediate::Undefined),
                    Value::from_immediate(Immediate::Undefined),
                ],
                count: 3,
            })?;
            if let Some(promise) =
                self.promise_resolve_same_constructor(callback_result, constructor)?
            {
                return self.finish_promise_finally_resolved(
                    NativeContinuation::promise_finally_resolve(
                        site,
                        Value::from_heap_ref(result_state.raw()),
                    ),
                    result_state,
                    promise,
                );
            }
            let parent = NativeContinuation::promise_finally_resolve(
                site,
                Value::from_heap_ref(result_state.raw()),
            );
            let completion_depth = self.fiber.completions.len();
            self.fiber
                .completions
                .push_native(parent)
                .map_err(Isolate::completion_stack_error)?;
            let resolution_site = NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site
                    .destination
                    .checked_add(1)
                    .ok_or(ExecutionError::BoundArgumentCountOverflow)?,
                call_site: site.call_site,
            };
            let resolve_site = CallSite {
                caller_base: resolution_site.caller_base,
                destination: resolution_site.destination,
                callee: self
                    .realm
                    .promise_resolve
                    .expect("Promise.resolve initializes before finally"),
                argument_base: 0,
                argument_source: None,
                argument_prefix: None,
                argument_prefix_offset: 0,
                argument_prefix_count: 0,
                argument_count: 0,
                this_value: constructor,
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: site.call_site,
            };
            self.begin_generic_promise_resolve(&resolve_site, constructor, callback_result)?;
            if self.fiber.frames.len() == 1 && self.fiber.completions.len() == completion_depth + 1
            {
                let parent = self.pop_native_continuation()?;
                let promise =
                    self.read(resolution_site.caller_base, resolution_site.destination)?;
                return self.finish_promise_finally_resolved(parent, result_state, promise);
            }
            return Ok(());
        }
        if let Some(promise) = self.promise_resolve_same_constructor(callback_result, intrinsic)? {
            return self.begin_promise_finally_mapping(site, promise, original, rejected);
        }
        let callback_promise = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.begin_promise_resolution(
            callback_promise,
            callback_result,
            site,
            PromiseResolutionMode::Internal,
        )?;
        self.begin_promise_finally_mapping(site, callback_promise, original, rejected)?;
        Ok(())
    }

    /// Implements the PromiseResolve same-constructor identity fast path for native Promises.
    fn promise_resolve_same_constructor(
        &mut self,
        value: Value,
        constructor: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        if !self.is_object_value(value) || self.promise_snapshot(value).is_err() {
            return Ok(None);
        }
        let key = self.constructor_atom()?;
        let observed = self.get_data_property(value, key)?;
        Ok(observed
            .filter(|observed| *observed == constructor)
            .map(|_| value))
    }

    /// Attaches the final value/reason thunk after a custom PromiseResolve has produced its promise.
    pub(crate) fn finish_promise_finally_resolved(
        &mut self,
        continuation: NativeContinuation,
        state: GcRef<NativeCallState>,
        promise: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let original = pending.values[FIN_RESULT_ORIGINAL];
        let rejected = pending.values[FIN_RESULT_REJECTED].as_immediate() == Some(Immediate::True);
        self.begin_promise_finally_mapping(continuation.site(), promise, original, rejected)
    }

    /// Invokes the resolved callback promise's observable `then` with the restoration thunks.
    fn begin_promise_finally_mapping(
        &mut self,
        site: NativeContinuationSite,
        promise: Value,
        original: Value,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        let (handler, promise) =
            self.allocate_promise_finally_result_handler(original, rejected, promise)?;
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [
                promise,
                Value::from_immediate(Immediate::Undefined),
                handler,
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
            ],
            count: 4,
        })?;
        let then_atom = self.intern_intrinsic_name(b"then")?;
        self.dispatch_promise_finally_get(
            site,
            state,
            PromiseFinallyMethodStage::Then,
            promise,
            then_atom.into(),
        )
    }

    /// Creates the intrinsic result capability and publishes or enqueues both reactions.
    pub(crate) fn perform_intrinsic_promise_then(
        &mut self,
        source: Value,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
        site: NativeContinuationSite,
    ) -> Result<Value, ExecutionError> {
        let result = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        self.perform_promise_then_with_capability(source, on_fulfilled, on_rejected, result)?;
        Ok(result)
    }

    /// Publishes both reactions using either a direct Promise or a generic capability record.
    pub(crate) fn perform_promise_then_with_capability(
        &mut self,
        source: Value,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
        capability: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.promise_snapshot(source)?;
        match snapshot.state {
            PromiseState::Pending => {
                self.append_promise_reaction(source, capability, on_fulfilled, false)?;
                self.append_promise_reaction(source, capability, on_rejected, true)?;
            }
            PromiseState::Fulfilled => self.promise_jobs.push(PromiseJob::Reaction {
                handler: on_fulfilled.unwrap_or(Value::from_immediate(Immediate::Undefined)),
                capability,
                argument: snapshot.result,
                rejected: false,
            }),
            PromiseState::Rejected => self.promise_jobs.push(PromiseJob::Reaction {
                handler: on_rejected.unwrap_or(Value::from_immediate(Immediate::Undefined)),
                capability,
                argument: snapshot.result,
                rejected: true,
            }),
        }
        Ok(())
    }

    /// Appends one fixed reaction node and records both Promise and tail-node barriers.
    fn append_promise_reaction(
        &mut self,
        source: Value,
        capability: Value,
        handler: Option<Value>,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        let handler = handler.unwrap_or(Value::from_immediate(Immediate::Undefined));
        let mut roots = PromiseReactionRoots {
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
            source,
            capability,
            handler,
        };
        let reaction = self
            .heap
            .try_allocate_with_gc(
                self.types.promise_reaction,
                0,
                0,
                PromiseReaction {
                    handler: roots.handler,
                    capability: roots.capability,
                    next: None,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let raw = roots
            .source
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(roots.source))?;
        let source = self
            .heap
            .checked_reference(raw, self.types.promise_object)
            .map_err(|_| ExecutionError::NotObject(roots.source))?;
        self.heap.with_running_scope(|scope| {
            let source = scope.root(source).map_err(ExecutionError::Root)?;
            let reaction = scope.root(reaction).map_err(ExecutionError::Root)?;
            let old_tail = scope.with_no_gc_scope(|no_gc| {
                let promise = no_gc
                    .borrow_mut(source, self.types.promise_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let (head, tail) = if rejected {
                    (&mut promise.reject_head, &mut promise.reject_tail)
                } else {
                    (&mut promise.fulfill_head, &mut promise.fulfill_tail)
                };
                let old_tail = *tail;
                if head.is_none() {
                    *head = Some(reaction.as_gc_ref());
                }
                *tail = Some(reaction.as_gc_ref());
                Ok(old_tail)
            })?;
            scope
                .write_barrier(source, reaction)
                .map_err(ExecutionError::HeapReference)?;
            if let Some(old_tail) = old_tail {
                let old_tail = scope.root(old_tail).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(old_tail, self.types.promise_reaction)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .next = Some(reaction.as_gc_ref());
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(old_tail, reaction)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Copies linked reactions into FIFO jobs after a Promise transitions out of pending.
    fn enqueue_promise_reaction_list(
        &mut self,
        mut reaction: Option<GcRef<PromiseReaction>>,
        argument: Value,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        while let Some(current) = reaction {
            let snapshot = self.heap.with_running_scope(|scope| {
                let current = scope.root(current).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(current, self.types.promise_reaction)
                        .copied()
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            self.promise_jobs.push(PromiseJob::Reaction {
                handler: snapshot.handler,
                capability: snapshot.capability,
                argument,
                rejected,
            });
            reaction = snapshot.next;
        }
        Ok(())
    }

    /// Drains a complete ECMAScript checkpoint for synchronous embedding entry points.
    pub(crate) fn promise_checkpoint(
        &mut self,
        result: Value,
        return_site: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            match self.promise_checkpoint_step(result, return_site)? {
                PromiseCheckpointProgress::Progressed => {}
                PromiseCheckpointProgress::Suspended => return Ok(None),
                PromiseCheckpointProgress::Completed(outcome) => return Ok(Some(outcome)),
            }
        }
    }

    /// Claims and advances at most one Promise or finalization job.
    pub(crate) fn promise_checkpoint_step(
        &mut self,
        result: Value,
        return_site: WordOffset,
    ) -> Result<PromiseCheckpointProgress, ExecutionError> {
        self.promise_jobs.begin_checkpoint(result);
        let Some(job) = self.promise_jobs.begin_next() else {
            if self.begin_finalization_cleanup_job(return_site)? {
                return Ok(PromiseCheckpointProgress::Suspended);
            }
            if self.finalization_jobs.has_pending_work(&self.heap) {
                return Ok(PromiseCheckpointProgress::Progressed);
            }
            let result = self
                .promise_jobs
                .finish_checkpoint()
                .expect("checkpoint retains the original completion");
            return Ok(PromiseCheckpointProgress::Completed(RunOutcome::Completed(
                result,
            )));
        };
        let outcome = match job {
            PromiseJob::Reaction {
                handler,
                capability,
                argument,
                rejected,
            } => {
                if self.is_async_function_state(capability) {
                    self.resume_async_function_job(capability, argument, rejected, return_site)?
                } else if self.is_async_module_state(capability) {
                    self.resume_async_module_job(capability, argument, rejected)?
                } else if self.is_async_generator_await(capability) {
                    self.resume_async_generator_await_job(
                        capability,
                        argument,
                        rejected,
                        return_site,
                    )?
                } else if self.is_dynamic_import_promise(capability) {
                    self.resume_dynamic_import_job(capability, argument, rejected)?;
                    None
                } else if self.resolve_function_object(handler).is_err() {
                    let site = self.promise_job_site(return_site)?;
                    let suspended = self
                        .begin_promise_reaction_settlement(capability, argument, rejected, site)?;
                    return Ok(if suspended {
                        PromiseCheckpointProgress::Suspended
                    } else {
                        PromiseCheckpointProgress::Progressed
                    });
                } else {
                    self.call_promise_reaction_handler(handler, capability, argument, return_site)?
                }
            }
            PromiseJob::Thenable {
                promise,
                thenable,
                then,
            } => {
                return Ok(
                    if self.begin_promise_thenable_job(promise, thenable, then, return_site)? {
                        PromiseCheckpointProgress::Suspended
                    } else {
                        PromiseCheckpointProgress::Progressed
                    },
                );
            }
        };
        Ok(match outcome {
            Some(outcome) => PromiseCheckpointProgress::Completed(outcome),
            None if self.fiber.frames.is_empty() => PromiseCheckpointProgress::Progressed,
            None => PromiseCheckpointProgress::Suspended,
        })
    }

    /// Calls one reaction handler and leaves its active job rooted until continuation completion.
    fn call_promise_reaction_handler(
        &mut self,
        handler: Value,
        capability: Value,
        argument: Value,
        return_site: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let arguments = self.allocate_promise_job_arguments(argument)?;
        let site = self.promise_job_site(return_site)?;
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_reaction(site, capability))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: 0,
            callee: handler,
            argument_base: 0,
            argument_source: Some(arguments),
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: return_site,
        }) {
            let continuation = self
                .fiber
                .completions
                .pop_native()
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            if let ExecutionError::HostThrown(reason) = error {
                self.begin_promise_reaction_rejection(continuation, reason)?;
                return Ok(None);
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Promise reaction handler publishes a frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        let reaction_pending = self.fiber.completions.last_native().is_some_and(|entry| {
            entry.kind() == NativeContinuationKind::PromiseReaction && entry.first() == capability
        });
        if !reaction_pending {
            if self.fiber.completions.len() > completion_depth {
                return Ok(None);
            }
            return Ok(None);
        }
        debug_assert!(self.fiber.completions.len() > completion_depth);
        let continuation = self
            .fiber
            .completions
            .pop_native()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let returned = self.read(site.caller_base, site.destination)?;
        let settlement_frame_depth = self.fiber.frames.len();
        self.finish_promise_reaction(continuation, returned)?;
        if self.fiber.frames.len() != settlement_frame_depth {
            return Ok(None);
        }
        Ok(None)
    }

    /// Calls one thenable job with fresh resolving functions and no recursive interpreter entry.
    fn begin_promise_thenable_job(
        &mut self,
        promise: Value,
        thenable: Value,
        then: Value,
        return_site: WordOffset,
    ) -> Result<bool, ExecutionError> {
        let arguments = self.create_promise_capability_arguments(promise)?;
        let site = self.promise_job_site(return_site)?;
        let continuation = NativeContinuation::promise_thenable(
            site,
            promise,
            Value::from_heap_ref(arguments.raw()),
        );
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: 0,
            callee: then,
            argument_base: 0,
            argument_source: Some(arguments),
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 2,
            this_value: thenable,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: return_site,
        }) {
            let Some(kind) = execution_error_kind(&error) else {
                self.pop_native_continuation()?;
                return Err(error);
            };
            let reason = self.create_native_error(kind, None)?;
            self.pop_native_continuation()?;
            self.reject_promise_thenable(continuation, reason)?;
            return Ok(false);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("thenable bytecode call publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(true);
        }
        if self.fiber.completions.len() <= completion_depth {
            return Ok(false);
        }
        let continuation = self.pop_native_continuation()?;
        debug_assert_eq!(continuation.kind(), NativeContinuationKind::PromiseThenable);
        self.promise_jobs.finish_active();
        Ok(false)
    }

    /// Completes a normally returned thenable call and resumes the owning checkpoint boundary.
    pub(crate) fn finish_promise_thenable(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        self.promise_jobs.finish_active();
        if let Some(frame) = self.fiber.frames.last_mut() {
            frame.pc = continuation.site().call_site;
        }
        Ok(())
    }

    /// Routes an abrupt thenable call through its shared first-call-wins reject function.
    pub(crate) fn reject_promise_thenable(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let arguments = self.native_call_state_reference(continuation.second())?;
        let reject = self.native_call_state_snapshot(arguments)?.values[1];
        let FunctionExecutable::PromiseResolver { cell, reject: true } =
            self.resolve_function_object(reject)?.executable
        else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        if let Some(promise) = self.claim_promise_resolver(cell)? {
            self.settle_promise(promise, PromiseState::Rejected, reason)?;
        }
        self.promise_jobs.finish_active();
        if let Some(frame) = self.fiber.frames.last_mut() {
            frame.pc = continuation.site().call_site;
        }
        Ok(())
    }

    /// Provides one destination slot for a Promise job invoked without a JavaScript caller.
    fn promise_job_site(
        &mut self,
        return_site: WordOffset,
    ) -> Result<NativeContinuationSite, ExecutionError> {
        let caller_base = if let Some(frame) = self.fiber.frames.last() {
            frame.base
        } else {
            if self.fiber.registers.is_empty() {
                self.fiber
                    .registers
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
                self.fiber
                    .registers
                    .push(Value::from_immediate(Immediate::Undefined));
            }
            0
        };
        Ok(NativeContinuationSite {
            caller_base,
            destination: 0,
            call_site: return_site,
        })
    }

    /// Rejects an abruptly completed Promise executor through its shared one-shot cell.
    pub(crate) fn reject_promise_executor(
        &mut self,
        continuation: NativeContinuation,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let arguments = self.native_call_state_reference(continuation.second())?;
        let reject = self.native_call_state_snapshot(arguments)?.values[1];
        let FunctionExecutable::PromiseResolver { cell, reject: true } =
            self.resolve_function_object(reject)?.executable
        else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        if let Some(promise) = self.claim_promise_resolver(cell)? {
            self.settle_promise(promise, PromiseState::Rejected, reason)?;
        }
        Ok(())
    }
}

/// Allocates one resolver while capability siblings remain in the caller-owned root set.
fn allocate_promise_resolver(
    heap: &mut Heap,
    function_type: GcType<FunctionObject>,
    cell: GcRef<PromiseResolutionCell>,
    reject: bool,
    prototype: Value,
    roots: &mut PromiseCapabilityRoots<'_>,
) -> Result<Value, ExecutionError> {
    heap.try_allocate_with_gc(
        function_type,
        0,
        0,
        FunctionObject {
            executable: FunctionExecutable::PromiseResolver { cell, reject },
            prototype_or_home_object: None,
            ordinary: OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype,
            },
        },
        AllocationSpace::Young,
        roots,
    )
    .map(|function| Value::from_heap_ref(function.raw()))
    .map_err(ExecutionError::HeapAllocation)
}
