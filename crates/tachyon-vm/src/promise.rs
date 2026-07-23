//! Promise state, reaction records, and the isolate-owned FIFO microtask substrate.

use super::*;

const FIN_SOURCE: usize = 0;
const FIN_CALLBACK: usize = 1;
const FIN_FULFILLED: usize = 2;
const FIN_REJECTED: usize = 3;
const FIN_CONSTRUCTOR: usize = 4;
const FIN_RESULT_ORIGINAL: usize = 0;
const FIN_RESULT_REJECTED: usize = 2;

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
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
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
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
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
        let prototype = self
            .get_data_property(site.new_target, prototype_atom)?
            .filter(|prototype| self.is_object_value(*prototype))
            .unwrap_or(
                self.realm
                    .promise_prototype
                    .expect("Promise prototype initializes before construction"),
            );
        let promise = self.create_promise_with_prototype(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
            prototype,
        )?;
        self.write(site.caller_base, site.destination, promise)?;
        let arguments = self.create_promise_capability_arguments(promise)?;
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
            PromiseResolutionMode::Reaction => {
                self.promise_jobs.finish_active();
                self.fiber
                    .frames
                    .last_mut()
                    .ok_or(ExecutionError::MissingEnvironment)?
                    .pc = site.call_site;
                Ok(())
            }
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
    pub(crate) fn promise_catch(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let on_rejected = self
            .call_argument(site, 0)?
            .filter(|value| self.resolve_function_object(*value).is_ok());
        self.perform_intrinsic_promise_then(
            site.this_value,
            None,
            on_rejected,
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
        )
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
                let (on_fulfilled, on_rejected) = if callable {
                    (
                        self.allocate_promise_finally_handler(callback, constructor, false)?,
                        self.allocate_promise_finally_handler(callback, constructor, true)?,
                    )
                } else {
                    (callback, callback)
                };
                self.set_promise_then_value(state, FIN_FULFILLED, on_fulfilled)?;
                self.set_promise_then_value(state, FIN_REJECTED, on_rejected)?;
                let then_atom = self.intern_intrinsic_name(b"then")?;
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
                let pending = self.native_call_state_snapshot(state)?;
                let arguments = self.allocate_promise_then_state(NativeCallState {
                    values: [
                        pending.values[FIN_FULFILLED],
                        pending.values[FIN_REJECTED],
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                    ],
                    count: 2,
                })?;
                let continuation = NativeContinuation::promise_finally_method(
                    site,
                    PromiseFinallyMethodStage::ThenCall,
                    Value::from_heap_ref(state.raw()),
                    value,
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
                    callee: value,
                    argument_base: 0,
                    argument_source: Some(arguments),
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: 2,
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
                    self.write(site.caller_base, site.destination, result)?;
                } else {
                    return Ok(());
                }
                Ok(())
            }
            PromiseFinallyMethodStage::ThenCall => {
                self.write(site.caller_base, site.destination, value)
            }
        }
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
        let callback_promise = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        let resolution_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site
                .destination
                .checked_add(1)
                .ok_or(ExecutionError::BoundArgumentCountOverflow)?,
            call_site: site.call_site,
        };
        self.begin_promise_resolution(
            callback_promise,
            callback_result,
            resolution_site,
            PromiseResolutionMode::StaticResolve,
        )?;
        self.begin_promise_finally_mapping(site, callback_promise, original, rejected)?;
        Ok(())
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
        let value_handler = self.allocate_promise_finally_result_handler(original, rejected)?;
        let throw_handler = self.allocate_promise_finally_result_handler(original, true)?;
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [
                promise,
                Value::from_immediate(Immediate::Undefined),
                value_handler,
                throw_handler,
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
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
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

    /// Drains Promise reactions at one ECMAScript job boundary without recursive VM entry.
    pub(crate) fn promise_checkpoint(
        &mut self,
        result: Value,
        return_site: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.promise_jobs.begin_checkpoint(result);
        loop {
            let Some(job) = self.promise_jobs.begin_next() else {
                let result = self
                    .promise_jobs
                    .finish_checkpoint()
                    .expect("checkpoint retains the original completion");
                return Ok(Some(RunOutcome::Completed(result)));
            };
            match job {
                PromiseJob::Reaction {
                    handler,
                    capability,
                    argument,
                    rejected,
                } => {
                    if self.resolve_function_object(handler).is_err() {
                        if self.begin_promise_reaction_settlement(
                            capability,
                            argument,
                            rejected,
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
                        )? {
                            return Ok(None);
                        }
                        continue;
                    }
                    return self.call_promise_reaction_handler(
                        handler,
                        capability,
                        argument,
                        return_site,
                    );
                }
                PromiseJob::Thenable {
                    promise,
                    thenable,
                    then,
                } => {
                    if self.begin_promise_thenable_job(promise, thenable, then, return_site)? {
                        return Ok(None);
                    }
                }
            }
        }
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
        let frame = *self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::MissingEnvironment)?;
        let site = NativeContinuationSite {
            caller_base: frame.base,
            destination: 0,
            call_site: return_site,
        };
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_reaction(site, capability))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: frame.base,
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
        if self.fiber.completions.len() <= completion_depth {
            return self.promise_checkpoint(
                self.promise_jobs
                    .checkpoint_result
                    .expect("completed reaction retains its checkpoint result"),
                return_site,
            );
        }
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
        self.promise_checkpoint(
            self.promise_jobs
                .checkpoint_result
                .expect("active checkpoint retains its result"),
            return_site,
        )
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
        let frame = *self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::MissingEnvironment)?;
        let site = NativeContinuationSite {
            caller_base: frame.base,
            destination: 0,
            call_site: return_site,
        };
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
            caller_base: frame.base,
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
        self.fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?
            .pc = continuation.site().call_site;
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
        self.fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?
            .pc = continuation.site().call_site;
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
