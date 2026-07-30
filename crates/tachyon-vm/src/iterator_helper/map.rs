//! Resumable `Iterator.prototype.map` creation, stepping, and close semantics.

use super::super::*;
use super::{IteratorHelperKind, IteratorHelperState};

struct IteratorHelperAllocationRoots<'a> {
    vm: VmRoots<'a>,
    iterator: Value,
    next_method: Value,
    callback: Value,
    prototype: Value,
}

impl Trace for IteratorHelperAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.iterator.trace(tracer);
        self.next_method.trace(tracer);
        self.callback.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Isolate {
    /// Validates map's receiver and mapper before observing the direct next method.
    pub(crate) fn begin_iterator_map(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let iterator = site.this_value;
        if !self.is_object_value(iterator) {
            return Err(ExecutionError::NotObject(iterator));
        }
        let mapper = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let native_site = Self::native_site(site);
        if !self.is_callable_value(mapper)? {
            self.write(site.caller_base, site.destination, iterator)?;
            let original = self.create_native_error(NativeErrorKind::Type, None)?;
            let iterator = self.read(site.caller_base, site.destination)?;
            return self.begin_iterator_helper_throw_close(
                native_site,
                IteratorHelperStage::CreateCloseReturnGet,
                iterator,
                original,
            );
        }
        let next = self.intern_intrinsic_name(b"next")?;
        self.dispatch_iterator_helper_get(
            native_site,
            IteratorHelperStage::CreateNextGet,
            iterator,
            mapper,
            iterator,
            next.into(),
        )
    }

    /// Implements `%IteratorHelperPrototype%.next` for the current lazy helper kinds.
    pub(crate) fn begin_iterator_helper_next(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let helper = site.this_value;
        let reference = self.iterator_helper_reference(helper)?;
        let mut snapshot = self.iterator_helper_snapshot(reference)?;
        match snapshot.state {
            IteratorHelperState::Executing => {
                return Err(ExecutionError::NotObject(helper));
            }
            IteratorHelperState::Completed => {
                let result =
                    self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
                return self.write(site.caller_base, site.destination, result);
            }
            IteratorHelperState::SuspendedYield => {
                snapshot.counter_or_limit = snapshot
                    .counter_or_limit
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
                self.set_iterator_helper_counter(reference, snapshot.counter_or_limit)?;
            }
            IteratorHelperState::SuspendedStart => {}
        }
        self.set_iterator_helper_state(reference, IteratorHelperState::Executing)?;
        self.call_iterator_helper(
            Self::native_site(site),
            IteratorHelperStage::NextCall,
            helper,
            snapshot.outer_next,
            snapshot.outer_iterator,
            Value::from_immediate(Immediate::Undefined),
            &[],
        )
    }

    /// Implements helper return with normal IteratorClose precedence.
    pub(crate) fn begin_iterator_helper_return(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let helper = site.this_value;
        let reference = self.iterator_helper_reference(helper)?;
        let snapshot = self.iterator_helper_snapshot(reference)?;
        if snapshot.state == IteratorHelperState::Executing {
            return Err(ExecutionError::NotObject(helper));
        }
        if snapshot.state == IteratorHelperState::Completed {
            let result =
                self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
            return self.write(site.caller_base, site.destination, result);
        }
        self.set_iterator_helper_state(reference, IteratorHelperState::Completed)?;
        let return_key = self.intern_intrinsic_name(b"return")?;
        self.dispatch_iterator_helper_get(
            Self::native_site(site),
            IteratorHelperStage::NormalCloseReturnGet,
            helper,
            Value::from_immediate(Immediate::Undefined),
            snapshot.outer_iterator,
            return_key.into(),
        )
    }

    /// Resumes one map/helper boundary from the interpreter's typed continuation loop.
    pub(crate) fn resume_iterator_helper(
        &mut self,
        continuation: NativeContinuation,
        stage: IteratorHelperStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        match stage {
            IteratorHelperStage::CreateNextGet => {
                let helper = self.allocate_iterator_map_helper(
                    continuation.first(),
                    value,
                    continuation.second(),
                )?;
                self.write(site.caller_base, site.destination, helper)
            }
            IteratorHelperStage::NextCall => {
                if !self.is_object_value(value) {
                    self.complete_iterator_helper(continuation.first())?;
                    return Err(ExecutionError::NotObject(value));
                }
                let done = self.intern_intrinsic_name(b"done")?;
                self.dispatch_iterator_helper_get(
                    site,
                    IteratorHelperStage::DoneGet,
                    continuation.first(),
                    value,
                    value,
                    done.into(),
                )
            }
            IteratorHelperStage::DoneGet => {
                let helper = continuation.first();
                if self.is_truthy_value(value)? {
                    self.complete_iterator_helper(helper)?;
                    let result = self.create_iterator_result(
                        Value::from_immediate(Immediate::Undefined),
                        true,
                    )?;
                    return self.write(site.caller_base, site.destination, result);
                }
                let result = continuation.second();
                let value_key = self.intern_intrinsic_name(b"value")?;
                self.dispatch_iterator_helper_get(
                    site,
                    IteratorHelperStage::ValueGet,
                    helper,
                    result,
                    result,
                    value_key.into(),
                )
            }
            IteratorHelperStage::ValueGet => {
                let helper = continuation.first();
                let snapshot = self.iterator_helper_value_snapshot(helper)?;
                debug_assert_eq!(snapshot.kind, IteratorHelperKind::Map);
                let counter = safe_integer_value(snapshot.counter_or_limit);
                self.call_iterator_helper(
                    site,
                    IteratorHelperStage::MapCallbackCall,
                    helper,
                    snapshot.callback,
                    Value::from_immediate(Immediate::Undefined),
                    Value::from_immediate(Immediate::Undefined),
                    &[value, counter],
                )
            }
            IteratorHelperStage::MapCallbackCall => {
                let helper = continuation.first();
                let reference = self.iterator_helper_reference(helper)?;
                self.set_iterator_helper_state(reference, IteratorHelperState::SuspendedYield)?;
                let result = self.create_iterator_result(value, false)?;
                self.write(site.caller_base, site.destination, result)
            }
            IteratorHelperStage::CreateCloseReturnGet
            | IteratorHelperStage::AbruptCloseReturnGet => {
                self.resume_iterator_helper_throw_close(continuation, stage, value)
            }
            IteratorHelperStage::CreateCloseReturnCall
            | IteratorHelperStage::AbruptCloseReturnCall => {
                Err(ExecutionError::HostThrown(continuation.second()))
            }
            IteratorHelperStage::NormalCloseReturnGet => {
                self.resume_iterator_helper_normal_close_get(continuation, value)
            }
            IteratorHelperStage::NormalCloseReturnCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.finish_iterator_helper_done(site)
            }
        }
    }

    /// Handles a thrown JS value at a helper callback/getter boundary.
    pub(crate) fn handle_iterator_helper_thrown(
        &mut self,
        continuation: NativeContinuation,
        thrown: Value,
    ) -> Result<Option<Option<RunOutcome>>, ExecutionError> {
        let Some(parent) = self.iterator_helper_effective_continuation(continuation) else {
            return Ok(None);
        };
        let NativeContinuationKind::IteratorHelper(stage) = parent.kind() else {
            return Ok(None);
        };
        let site = parent.site();
        match stage {
            IteratorHelperStage::MapCallbackCall => {
                let helper = parent.first();
                self.complete_iterator_helper(helper)?;
                self.begin_iterator_helper_throw_close(
                    site,
                    IteratorHelperStage::AbruptCloseReturnGet,
                    helper,
                    thrown,
                )?;
                Ok(Some(None))
            }
            IteratorHelperStage::CreateCloseReturnGet
            | IteratorHelperStage::CreateCloseReturnCall
            | IteratorHelperStage::AbruptCloseReturnGet
            | IteratorHelperStage::AbruptCloseReturnCall => {
                self.throw_value(parent.second(), site.call_site).map(Some)
            }
            IteratorHelperStage::NextCall
            | IteratorHelperStage::DoneGet
            | IteratorHelperStage::ValueGet => {
                self.complete_iterator_helper(parent.first())?;
                self.throw_value(thrown, site.call_site).map(Some)
            }
            IteratorHelperStage::CreateNextGet
            | IteratorHelperStage::NormalCloseReturnGet
            | IteratorHelperStage::NormalCloseReturnCall => {
                self.throw_value(thrown, site.call_site).map(Some)
            }
        }
    }

    /// Converts an immediate native callback error into map's explicit close policy.
    fn handle_iterator_helper_call_error(
        &mut self,
        continuation: NativeContinuation,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let NativeContinuationKind::IteratorHelper(stage) = continuation.kind() else {
            return Err(error);
        };
        match stage {
            IteratorHelperStage::MapCallbackCall => {
                let site = continuation.site();
                let helper = continuation.first();
                self.complete_iterator_helper(helper)?;
                self.write(site.caller_base, site.destination, helper)?;
                let thrown = match error {
                    ExecutionError::HostThrown(value) => value,
                    error => {
                        let Some(kind) = execution_error_kind(&error) else {
                            return Err(error);
                        };
                        self.create_native_error(kind, None)?
                    }
                };
                let helper = self.read(site.caller_base, site.destination)?;
                self.begin_iterator_helper_throw_close(
                    site,
                    IteratorHelperStage::AbruptCloseReturnGet,
                    helper,
                    thrown,
                )
            }
            IteratorHelperStage::NextCall => {
                self.complete_iterator_helper(continuation.first())?;
                Err(error)
            }
            IteratorHelperStage::CreateCloseReturnCall
            | IteratorHelperStage::AbruptCloseReturnCall => {
                Err(ExecutionError::HostThrown(continuation.second()))
            }
            _ => Err(error),
        }
    }

    /// Starts throw-completion IteratorClose for creation validation or mapper abrupt.
    fn begin_iterator_helper_throw_close(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        owner: Value,
        original: Value,
    ) -> Result<(), ExecutionError> {
        let iterator = if stage == IteratorHelperStage::CreateCloseReturnGet {
            owner
        } else {
            self.iterator_helper_value_snapshot(owner)?.outer_iterator
        };
        let return_key = self.intern_intrinsic_name(b"return")?;
        self.dispatch_iterator_helper_get(site, stage, owner, original, iterator, return_key.into())
    }

    /// Applies throw-completion precedence after observable return lookup.
    fn resume_iterator_helper_throw_close(
        &mut self,
        continuation: NativeContinuation,
        stage: IteratorHelperStage,
        method: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        if is_nullish(method) || !self.is_callable_value(method)? {
            return Err(ExecutionError::HostThrown(continuation.second()));
        }
        let iterator = if stage == IteratorHelperStage::CreateCloseReturnGet {
            continuation.first()
        } else {
            self.iterator_helper_value_snapshot(continuation.first())?
                .outer_iterator
        };
        let call_stage = if stage == IteratorHelperStage::CreateCloseReturnGet {
            IteratorHelperStage::CreateCloseReturnCall
        } else {
            IteratorHelperStage::AbruptCloseReturnCall
        };
        self.call_iterator_helper(
            site,
            call_stage,
            continuation.first(),
            method,
            iterator,
            continuation.second(),
            &[],
        )
    }

    /// Applies normal-completion IteratorClose and validates its return object.
    fn resume_iterator_helper_normal_close_get(
        &mut self,
        continuation: NativeContinuation,
        method: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        if is_nullish(method) {
            return self.finish_iterator_helper_done(site);
        }
        self.resolve_function_object(method)?;
        let iterator = self
            .iterator_helper_value_snapshot(continuation.first())?
            .outer_iterator;
        self.call_iterator_helper(
            site,
            IteratorHelperStage::NormalCloseReturnCall,
            continuation.first(),
            method,
            iterator,
            Value::from_immediate(Immediate::Undefined),
            &[],
        )
    }

    /// Performs a resumable property Get with the helper operation as its parent.
    fn dispatch_iterator_helper_get(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        first: Value,
        second: Value,
        target: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_helper(
                site, stage, first, second,
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_aware_property_read(site, target, target, key);
        if let Err(error) = outcome {
            let continuation = self.pop_native_continuation()?;
            if matches!(
                stage,
                IteratorHelperStage::NextCall
                    | IteratorHelperStage::DoneGet
                    | IteratorHelperStage::ValueGet
            ) {
                self.complete_iterator_helper(first)?;
            }
            if matches!(
                stage,
                IteratorHelperStage::CreateCloseReturnGet
                    | IteratorHelperStage::AbruptCloseReturnGet
            ) {
                return Err(ExecutionError::HostThrown(continuation.second()));
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.resume_iterator_helper(continuation, stage, returned)
    }

    /// Calls one cached iterator method or mapper through an immutable exact argument prefix.
    #[allow(
        clippy::too_many_arguments,
        reason = "the typed call boundary keeps stage, roots, receiver, and exact arguments explicit"
    )]
    fn call_iterator_helper(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        owner: Value,
        callee: Value,
        receiver: Value,
        retained: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(callee)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_helper(
                site, stage, owner, retained,
            ))
            .map_err(Self::completion_stack_error)?;
        let prefix = if arguments.is_empty() {
            None
        } else {
            let mut copied = Vec::new();
            copied
                .try_reserve_exact(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
            copied.extend_from_slice(arguments);
            Some(self.create_apply_argument_prefix(callee, receiver, copied)?)
        };
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: prefix,
            argument_prefix_offset: 0,
            argument_prefix_count: arguments.len() as u32,
            argument_count: arguments.len() as u32,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            let continuation = self.pop_native_continuation()?;
            return self.handle_iterator_helper_call_error(continuation, error);
        }
        let parent_is_active = self.fiber.completions.last_native().is_some_and(|parent| {
            matches!(parent.kind(), NativeContinuationKind::IteratorHelper(parent_stage) if parent_stage == stage)
                && parent.first() == owner
        });
        if !parent_is_active {
            return Ok(());
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Iterator Helper callback publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.resume_iterator_helper(continuation, stage, returned)
    }

    /// Allocates the fixed-layout map helper after its cached next Get succeeds.
    fn allocate_iterator_map_helper(
        &mut self,
        iterator: Value,
        next_method: Value,
        callback: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .iterator_helper_prototype
            .expect("Iterator Helper prototype initializes before map");
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = IteratorHelperAllocationRoots {
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
            iterator,
            next_method,
            callback,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.iterator_helper,
                0,
                0,
                IteratorHelperObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    outer_iterator: roots.iterator,
                    outer_next: roots.next_method,
                    callback: roots.callback,
                    inner_iterator: undefined,
                    inner_next: undefined,
                    counter_or_limit: 0,
                    kind: IteratorHelperKind::Map,
                    state: IteratorHelperState::SuspendedStart,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|helper| Value::from_heap_ref(helper.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns the helper continuation itself or the parent below a generic getter callback.
    fn iterator_helper_effective_continuation(
        &self,
        continuation: NativeContinuation,
    ) -> Option<NativeContinuation> {
        if matches!(
            continuation.kind(),
            NativeContinuationKind::IteratorHelper(_)
        ) {
            Some(continuation)
        } else {
            self.fiber
                .completions
                .last_native()
                .filter(|parent| matches!(parent.kind(), NativeContinuationKind::IteratorHelper(_)))
        }
    }

    /// Reads the branded helper payload by value before leaving a no-GC borrow.
    fn iterator_helper_value_snapshot(
        &mut self,
        helper: Value,
    ) -> Result<IteratorHelperObject, ExecutionError> {
        let reference = self.iterator_helper_reference(helper)?;
        self.iterator_helper_snapshot(reference)
    }

    fn iterator_helper_reference(
        &self,
        helper: Value,
    ) -> Result<GcRef<IteratorHelperObject>, ExecutionError> {
        let raw = helper
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(helper))?;
        self.heap
            .checked_reference(raw, self.types.iterator_helper)
            .map_err(|_| ExecutionError::NotObject(helper))
    }

    fn iterator_helper_snapshot(
        &mut self,
        helper: GcRef<IteratorHelperObject>,
    ) -> Result<IteratorHelperObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let helper = scope.root(helper).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(helper, self.types.iterator_helper)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_iterator_helper_state(
        &mut self,
        helper: GcRef<IteratorHelperObject>,
        state: IteratorHelperState,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let helper = scope.root(helper).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(helper, self.types.iterator_helper)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.state = state;
                Ok(())
            })
        })
    }

    fn set_iterator_helper_counter(
        &mut self,
        helper: GcRef<IteratorHelperObject>,
        counter: u64,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let helper = scope.root(helper).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(helper, self.types.iterator_helper)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.counter_or_limit = counter;
                Ok(())
            })
        })
    }

    fn complete_iterator_helper(&mut self, helper: Value) -> Result<(), ExecutionError> {
        let reference = self.iterator_helper_reference(helper)?;
        self.set_iterator_helper_state(reference, IteratorHelperState::Completed)
    }

    fn finish_iterator_helper_done(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<(), ExecutionError> {
        let result =
            self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
        self.write(site.caller_base, site.destination, result)
    }
}
