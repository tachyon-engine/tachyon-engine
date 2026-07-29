//! GC-managed Async-from-Sync Iterator objects and promise-backed protocol methods.

use super::*;

const STATE_ITERATOR: usize = 0;
const STATE_PROMISE: usize = 1;
const STATE_RESULT: usize = 2;
const STATE_ARGUMENT: usize = 3;
const STATE_FLAGS: usize = 4;

const OP_NEXT: i32 = 0;
const OP_RETURN: i32 = 1;
const OP_THROW: i32 = 2;
const ARGUMENT_PRESENT: i32 = 1 << 2;
const RESULT_DONE: i32 = 1 << 3;

/// Internal slots for one Async-from-Sync Iterator instance.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct AsyncFromSyncIteratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) sync_iterator: Value,
    pub(crate) next_method: Value,
}

impl Trace for AsyncFromSyncIteratorObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.sync_iterator.trace(tracer);
        self.next_method.trace(tracer);
    }
}

struct AsyncFromSyncAllocationRoots<'a> {
    vm: VmRoots<'a>,
    iterator: Value,
    next_method: Value,
    prototype: Value,
}

impl Trace for AsyncFromSyncAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.iterator.trace(tracer);
        self.next_method.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Isolate {
    /// Creates the specification wrapper while preserving the sync iterator's cached next method.
    pub(crate) fn create_async_from_sync_iterator(
        &mut self,
        iterator: Value,
        next_method: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .async_from_sync_iterator_prototype
            .expect("Async-from-Sync prototype initializes before wrapper allocation");
        let mut roots = AsyncFromSyncAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            iterator,
            next_method,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.async_from_sync_iterator,
                0,
                0,
                AsyncFromSyncIteratorObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    sync_iterator: roots.iterator,
                    next_method: roots.next_method,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Starts `%AsyncFromSyncIteratorPrototype%.next` without recursive interpreter entry.
    pub(crate) fn begin_async_from_sync_iterator_next(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_async_from_sync_iterator_method(site, OP_NEXT)
    }

    /// Starts `%AsyncFromSyncIteratorPrototype%.return` through an observable method lookup.
    pub(crate) fn begin_async_from_sync_iterator_return(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_async_from_sync_iterator_method(site, OP_RETURN)
    }

    /// Starts `%AsyncFromSyncIteratorPrototype%.throw` through an observable method lookup.
    pub(crate) fn begin_async_from_sync_iterator_throw(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_async_from_sync_iterator_method(site, OP_THROW)
    }

    /// Allocates the intrinsic capability before checking the receiver brand, as required by spec.
    fn begin_async_from_sync_iterator_method(
        &mut self,
        site: &CallSite,
        operation: i32,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let promise = self.create_promise(PromiseState::Pending, undefined)?;
        self.write(site.caller_base, site.destination, promise)?;
        let Some(object) = self.async_from_sync_iterator_reference(site.this_value) else {
            let error = self.create_native_error(NativeErrorKind::Type, None)?;
            self.settle_promise(promise, PromiseState::Rejected, error)?;
            return Ok(());
        };
        let _ = self.async_from_sync_iterator_snapshot(object)?;
        let argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let flags = operation
            | if site.argument_count == 0 {
                0
            } else {
                ARGUMENT_PRESENT
            };
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [
                site.this_value,
                promise,
                undefined,
                argument,
                Value::from_i32(flags),
            ],
            count: 5,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_async_from_sync_state(native_site, state)?;
        let wrapper = self.native_call_state_snapshot(state)?.values[STATE_ITERATOR];
        let object = self
            .async_from_sync_iterator_reference(wrapper)
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let snapshot = self.async_from_sync_iterator_snapshot(object)?;
        match operation {
            OP_NEXT => self.dispatch_async_from_sync_call(
                native_site,
                state,
                snapshot.next_method,
                snapshot.sync_iterator,
                AsyncFromSyncIteratorStage::IteratorCall,
            ),
            OP_RETURN | OP_THROW => {
                let name: &[u8] = if operation == OP_RETURN {
                    b"return".as_slice()
                } else {
                    b"throw".as_slice()
                };
                let stage = if operation == OP_RETURN {
                    AsyncFromSyncIteratorStage::ReturnGet
                } else {
                    AsyncFromSyncIteratorStage::ThrowGet
                };
                let key = self.intern_intrinsic_name(name)?;
                self.dispatch_async_from_sync_read(
                    native_site,
                    state,
                    snapshot.sync_iterator,
                    key.into(),
                    stage,
                )
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Advances the operation after one observable sync iterator boundary completes.
    pub(crate) fn resume_async_from_sync_iterator(
        &mut self,
        continuation: NativeContinuation,
        stage: AsyncFromSyncIteratorStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.native_call_state_reference(continuation.first())?;
        if !matches!(
            stage,
            AsyncFromSyncIteratorStage::DoneGet | AsyncFromSyncIteratorStage::PromiseConstructorGet
        ) {
            self.set_promise_then_value(state, STATE_RESULT, value)?;
        }
        self.root_async_from_sync_state(site, state)?;
        match stage {
            AsyncFromSyncIteratorStage::IteratorCall
            | AsyncFromSyncIteratorStage::ReturnCall
            | AsyncFromSyncIteratorStage::ThrowCall => {
                if !self.is_object_value(value) {
                    return self.reject_async_from_sync_type_error(site, state);
                }
                let done = self.intern_intrinsic_name(b"done")?;
                self.dispatch_async_from_sync_read(
                    site,
                    state,
                    value,
                    done.into(),
                    AsyncFromSyncIteratorStage::DoneGet,
                )
            }
            AsyncFromSyncIteratorStage::ReturnGet => {
                self.finish_async_from_sync_method_get(site, state, value, false)
            }
            AsyncFromSyncIteratorStage::ThrowGet => {
                self.finish_async_from_sync_method_get(site, state, value, true)
            }
            AsyncFromSyncIteratorStage::DoneGet => {
                let done = self.is_truthy_value(value)?;
                self.set_async_from_sync_done(state, done)?;
                let result = self.native_call_state_snapshot(state)?.values[STATE_RESULT];
                let key = self.intern_intrinsic_name(b"value")?;
                self.dispatch_async_from_sync_read(
                    site,
                    state,
                    result,
                    key.into(),
                    AsyncFromSyncIteratorStage::ValueGet,
                )
            }
            AsyncFromSyncIteratorStage::ValueGet => {
                self.finish_async_from_sync_value(site, state, value)
            }
            AsyncFromSyncIteratorStage::PromiseConstructorGet => {
                self.finish_async_from_sync_constructor_get(site, state, value)
            }
            AsyncFromSyncIteratorStage::PromiseResolve => {
                self.attach_async_from_sync_unwrap(site, state, value)
            }
        }
    }

    /// Handles absent return/throw methods or calls the observed method with the original argument.
    fn finish_async_from_sync_method_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        method: Value,
        throwing: bool,
    ) -> Result<(), ExecutionError> {
        if is_nullish(method) {
            if throwing {
                return self.reject_async_from_sync_type_error(site, state);
            }
            let snapshot = self.native_call_state_snapshot(state)?;
            let result = self.create_iterator_result(snapshot.values[STATE_ARGUMENT], true)?;
            let promise = snapshot.values[STATE_PROMISE];
            self.settle_promise(promise, PromiseState::Fulfilled, result)?;
            return self.write(site.caller_base, site.destination, promise);
        }
        self.resolve_function_object(method)?;
        let iterator_value = self.native_call_state_snapshot(state)?.values[STATE_ITERATOR];
        let iterator = self.async_from_sync_iterator_snapshot(
            self.async_from_sync_iterator_reference(iterator_value)
                .ok_or(ExecutionError::MissingNativeContinuation)?,
        )?;
        self.dispatch_async_from_sync_call(
            site,
            state,
            method,
            iterator.sync_iterator,
            if throwing {
                AsyncFromSyncIteratorStage::ThrowCall
            } else {
                AsyncFromSyncIteratorStage::ReturnCall
            },
        )
    }

    /// Starts the observable PromiseResolve constructor check for native Promise values.
    fn finish_async_from_sync_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.promise_snapshot(value).is_ok() {
            let constructor = self.constructor_atom()?;
            return self.dispatch_async_from_sync_read(
                site,
                state,
                value,
                constructor.into(),
                AsyncFromSyncIteratorStage::PromiseConstructorGet,
            );
        }
        self.resolve_async_from_sync_value(site, state, value)
    }

    /// Applies the PromiseResolve identity rule after the observable constructor lookup.
    fn finish_async_from_sync_constructor_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        let value = self.native_call_state_snapshot(state)?.values[STATE_RESULT];
        let intrinsic = self
            .realm
            .promise_constructor
            .expect("Promise constructor initializes before Async-from-Sync iteration");
        if constructor == intrinsic {
            return self.attach_async_from_sync_unwrap(site, state, value);
        }
        self.resolve_async_from_sync_value(site, state, value)
    }

    /// Creates the intrinsic PromiseResolve wrapper and resumes after thenable assimilation starts.
    fn resolve_async_from_sync_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let wrapper = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.fiber
            .completions
            .push_native(NativeContinuation::async_from_sync_iterator(
                site,
                AsyncFromSyncIteratorStage::PromiseResolve,
                Value::from_heap_ref(state.raw()),
                wrapper,
            ))
            .map_err(Self::completion_stack_error)?;
        self.begin_promise_resolution(wrapper, value, site, PromiseResolutionMode::StaticResolve)?;
        if self.fiber.completions.last_native().is_some_and(|entry| {
            entry.kind()
                == NativeContinuationKind::AsyncFromSyncIterator(
                    AsyncFromSyncIteratorStage::PromiseResolve,
                )
        }) {
            let continuation = self.pop_native_continuation()?;
            let state = self.native_call_state_reference(continuation.first())?;
            self.attach_async_from_sync_unwrap(site, state, continuation.second())?;
        }
        Ok(())
    }

    /// Attaches the done-capturing unwrap callback to the value wrapper and result capability.
    fn attach_async_from_sync_unwrap(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        wrapper: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let flags = snapshot.values[STATE_FLAGS]
            .as_i32()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_promise_then_value(state, STATE_RESULT, wrapper)?;
        self.root_async_from_sync_state(site, state)?;
        let callback = self.allocate_async_from_sync_unwrap(flags & RESULT_DONE != 0)?;
        self.set_promise_then_value(state, STATE_ARGUMENT, callback)?;
        let snapshot = self.native_call_state_snapshot(state)?;
        self.perform_promise_then_with_capability(
            snapshot.values[STATE_RESULT],
            Some(snapshot.values[STATE_ARGUMENT]),
            None,
            snapshot.values[STATE_PROMISE],
        )?;
        self.write(
            site.caller_base,
            site.destination,
            snapshot.values[STATE_PROMISE],
        )
    }

    /// Dispatches one arbitrary iterator method without using the Rust call stack as JS state.
    fn dispatch_async_from_sync_call(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        method: Value,
        receiver: Value,
        stage: AsyncFromSyncIteratorStage,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let flags = snapshot.values[STATE_FLAGS]
            .as_i32()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let argument_present = flags & ARGUMENT_PRESENT != 0;
        let prefix = if argument_present {
            Some(self.create_apply_argument_prefix(
                method,
                receiver,
                vec![snapshot.values[STATE_ARGUMENT]],
            )?)
        } else {
            None
        };
        let (method, receiver) = if let Some(prefix) = prefix {
            let bound = self.bound_function_snapshot(prefix)?;
            (bound.call_target, bound.bound_this)
        } else {
            (method, receiver)
        };
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::async_from_sync_iterator(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                method,
            ))
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: method,
            argument_base: 0,
            argument_source: None,
            argument_prefix: prefix,
            argument_prefix_offset: 0,
            argument_prefix_count: u32::from(argument_present),
            argument_count: u32::from(argument_present),
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            let continuation = self.pop_native_continuation()?;
            return self.reject_async_from_sync_error(continuation.site(), state, error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            if self.fiber.frames.len() != frames {
                self.fiber
                    .frames
                    .last_mut()
                    .expect("call frame exists")
                    .return_register = None;
                self.fiber
                    .frames
                    .last_mut()
                    .expect("call frame exists")
                    .return_continuation = true;
            }
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.resume_async_from_sync_iterator(continuation, stage, result)
    }

    /// Performs one observable property read while retaining the complete operation state.
    fn dispatch_async_from_sync_read(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        key: PropertyKey,
        stage: AsyncFromSyncIteratorStage,
    ) -> Result<(), ExecutionError> {
        let continuation = NativeContinuation::async_from_sync_iterator(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        match self.resolve_property_read_until_proxy(receiver, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                return self.resume_async_from_sync_iterator(
                    continuation,
                    stage,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                return self.resume_async_from_sync_iterator(continuation, stage, value);
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                return self.resume_async_from_sync_iterator(
                    continuation,
                    stage,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => {
                return self
                    .dispatch_property_callback(continuation, getter)
                    .map(|_| ());
            }
            PropertyReadResolution::Proxy(_) => {}
        }
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return self.reject_async_from_sync_error(site, state, error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_async_from_sync_iterator(continuation, stage, value)
    }

    /// Rejects the operation's result Promise with a fresh TypeError.
    fn reject_async_from_sync_type_error(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let reason = self.create_native_error(NativeErrorKind::Type, None)?;
        self.reject_async_from_sync(site, state, reason)
    }

    /// Preserves an explicit JavaScript throw, converting VM validation failures to native errors.
    fn reject_async_from_sync_error(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
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
        self.reject_async_from_sync(site, state, reason)
    }

    /// Settles and republishes the operation's already-created result Promise.
    pub(crate) fn reject_async_from_sync(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        let promise = self.native_call_state_snapshot(state)?.values[STATE_PROMISE];
        self.settle_promise(promise, PromiseState::Rejected, reason)?;
        self.write(site.caller_base, site.destination, promise)
    }

    /// Returns the managed wrapper reference when the receiver has the unforgeable brand.
    pub(crate) fn async_from_sync_iterator_reference(
        &self,
        value: Value,
    ) -> Option<GcRef<AsyncFromSyncIteratorObject>> {
        value.as_heap_ref().and_then(|raw| {
            self.heap
                .checked_reference(raw, self.types.async_from_sync_iterator)
                .ok()
        })
    }

    /// Copies the wrapper slots without retaining a heap borrow across an allocation.
    fn async_from_sync_iterator_snapshot(
        &mut self,
        object: GcRef<AsyncFromSyncIteratorObject>,
    ) -> Result<AsyncFromSyncIteratorObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.async_from_sync_iterator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Updates the compact done bit in the traced operation state.
    fn set_async_from_sync_done(
        &mut self,
        state: GcRef<NativeCallState>,
        done: bool,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let mut flags = snapshot.values[STATE_FLAGS]
            .as_i32()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        if done {
            flags |= RESULT_DONE;
        } else {
            flags &= !RESULT_DONE;
        }
        self.set_promise_then_value(state, STATE_FLAGS, Value::from_i32(flags))
    }

    /// Allocates the small native reaction that creates the final IteratorResult object.
    fn allocate_async_from_sync_unwrap(&mut self, done: bool) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before iterator unwrap callbacks");
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
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::AsyncFromSyncIteratorUnwrap { done },
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

    #[inline(always)]
    fn root_async_from_sync_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }
}
