//! Proxy identity, rooting, and ProxyCreate allocation substrate.

use super::*;

mod get;
mod get_own;
mod has;

pub(crate) use get::PROXY_GET_ACTIVE;

/// A Proxy has no ordinary-property base; every object internal method must use exotic dispatch.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ProxyObject {
    pub(crate) target: Value,
    pub(crate) handler: Value,
}

impl Trace for ProxyObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.handler.trace(tracer);
    }
}

struct ProxyAllocationRoots<'a> {
    vm: VmRoots<'a>,
    target: Value,
    handler: Value,
}

impl Trace for ProxyAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.target.trace(tracer);
        self.handler.trace(tracer);
    }
}

struct ProxyRevocableRoots<'a> {
    vm: VmRoots<'a>,
    proxy: Value,
    revoker: Value,
    prototype: Value,
    storage: Option<GcRef<PropertyStorage>>,
}

impl Trace for ProxyRevocableRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.proxy.trace(tracer);
        self.revoker.trace(tracer);
        self.prototype.trace(tracer);
        self.storage.trace(tracer);
    }
}

struct NativeCallStateRoots<'a> {
    vm: VmRoots<'a>,
    values: [Value; 5],
}

impl Trace for NativeCallStateRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.values.trace(tracer);
    }
}

const PROXY_TARGET_ARGUMENT: usize = 0;
const PROXY_PROTOTYPE_ARGUMENT: usize = 1;
pub(crate) const PROXY_ACTIVE_OBJECT: usize = 2;
const PROXY_RESULT_OBJECT: usize = 3;
const PROXY_HAS_KEY_ARGUMENT: usize = 1;
const PROXY_GET_OWN_DESCRIPTOR: usize = 3;
const PROXY_GET_OWN_TARGET_DESCRIPTOR: usize = 4;

impl Isolate {
    /// Validates ProxyCreate arguments before allocating the independently branded exotic payload.
    pub(crate) fn create_proxy_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let handler = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        if !self.is_object_value(handler) {
            return Err(ExecutionError::NotObject(handler));
        }
        let mut roots = ProxyAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            target,
            handler,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.proxy_object,
                0,
                0,
                ProxyObject {
                    target: roots.target,
                    handler: roots.handler,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|proxy| Value::from_heap_ref(proxy.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Creates the Proxy, stateful revoker, and exact two-property result for Proxy.revocable.
    pub(crate) fn create_revocable_proxy_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let proxy = self.create_proxy_from_site(site)?;
        self.write(site.caller_base, site.destination, proxy)?;
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before Proxy.revocable");
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Proxy.revocable");
        let proxy_atom = self.intern_intrinsic_name(b"proxy")?;
        let revoke_atom = self.intern_intrinsic_name(b"revoke")?;
        let mut roots = ProxyRevocableRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            proxy,
            revoker: Value::from_immediate(Immediate::Undefined),
            prototype: function_prototype,
            storage: None,
        };
        let revoker = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::ProxyRevoker(roots.proxy),
                    function_prototype: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.revoker = Value::from_heap_ref(revoker.raw());
        roots.prototype = object_prototype;
        let proxy_shape = self
            .shapes
            .transition_add(ShapeId::EMPTY, proxy_atom, PropertyAttributes::DEFAULT_DATA)
            .map_err(ExecutionError::Shape)?;
        let result_shape = self
            .shapes
            .transition_add(proxy_shape, revoke_atom, PropertyAttributes::DEFAULT_DATA)
            .map_err(ExecutionError::Shape)?;
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage::new(Box::new([roots.proxy, roots.revoker])),
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.storage = Some(storage);
        self.heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape: result_shape,
                    extensible: true,
                    storage: roots.storage,
                    prototype: roots.prototype,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|result| Value::from_heap_ref(result.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Clears a revoker's private edge before invalidating both Proxy slots without allocation.
    pub(crate) fn revoke_proxy_from_function(
        &mut self,
        revoker: Value,
    ) -> Result<(), ExecutionError> {
        let proxy = match self.resolve_function_object(revoker)?.executable {
            FunctionExecutable::ProxyRevoker(proxy) => proxy,
            _ => return Err(ExecutionError::NonCallable(revoker)),
        };
        if proxy.as_immediate() == Some(Immediate::Null) {
            return Ok(());
        }
        let revoker_raw = revoker
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(revoker))?;
        let revoker_ref = self
            .heap
            .checked_reference(revoker_raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(revoker))?;
        let proxy_raw = proxy.as_heap_ref().ok_or(ExecutionError::ProxyRevoked)?;
        let proxy_ref = self
            .heap
            .checked_reference(proxy_raw, self.types.proxy_object)
            .map_err(|_| ExecutionError::ProxyRevoked)?;
        self.heap.with_running_scope(|scope| {
            let revoker = scope.root(revoker_ref).map_err(ExecutionError::Root)?;
            let proxy = scope.root(proxy_ref).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                {
                    let function = no_gc
                        .borrow_mut(revoker, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    function.executable =
                        FunctionExecutable::ProxyRevoker(Value::from_immediate(Immediate::Null));
                }
                let proxy = no_gc
                    .borrow_mut(proxy, self.types.proxy_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                proxy.target = Value::from_immediate(Immediate::Null);
                proxy.handler = Value::from_immediate(Immediate::Null);
                Ok(())
            })
        })
    }

    /// Routes Object/Reflect.setPrototypeOf to the Proxy slow path after shared argument validation.
    pub(crate) fn dispatch_proxy_set_prototype_from_site(
        &mut self,
        site: &CallSite,
        mode: ProxySetPrototypeMode,
    ) -> Result<bool, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_proxy_value(target) {
            return Ok(false);
        }
        let prototype = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if prototype.as_immediate() != Some(Immediate::Null) && !self.is_object_value(prototype) {
            return Err(ExecutionError::NotObject(prototype));
        }
        self.dispatch_proxy_set_prototype(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            target,
            prototype,
            target,
            mode,
        )?;
        Ok(true)
    }

    /// Walks absent traps iteratively and publishes pending state only at an observable callback.
    fn dispatch_proxy_set_prototype(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        prototype: Value,
        result_object: Value,
        mode: ProxySetPrototypeMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"setPrototypeOf")?;
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    let success = self.ordinary_set_prototype_of(snapshot.target, prototype)?;
                    return self.finish_proxy_set_prototype(site, mode, result_object, success);
                }
                PropertyRead::Data(trap) => {
                    let state = self.allocate_proxy_set_prototype_state(
                        snapshot.target,
                        prototype,
                        proxy,
                        result_object,
                    )?;
                    return self.continue_proxy_set_prototype_lookup(site, mode, state, trap);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    let success = self.ordinary_set_prototype_of(snapshot.target, prototype)?;
                    return self.finish_proxy_set_prototype(site, mode, result_object, success);
                }
                PropertyRead::Accessor(getter) => {
                    let state = self.allocate_proxy_set_prototype_state(
                        snapshot.target,
                        prototype,
                        proxy,
                        result_object,
                    )?;
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_set_prototype(
                            site,
                            mode,
                            ProxySetPrototypeStage::TrapGetter,
                            Value::from_heap_ref(state.raw()),
                            snapshot.handler,
                        ),
                        getter,
                    );
                }
            }
        }
    }

    /// Resumes trap lookup/call and target invariant checks from one typed continuation stage.
    pub(crate) fn resume_proxy_set_prototype(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxySetPrototypeMode,
        stage: ProxySetPrototypeStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ProxySetPrototypeStage::TrapGetter => {
                self.continue_proxy_set_prototype_lookup(continuation.site(), mode, state, value)
            }
            ProxySetPrototypeStage::TrapCall => {
                self.finish_proxy_set_prototype_trap(continuation.site(), mode, state, value)
            }
            ProxySetPrototypeStage::TargetIsExtensible => {
                self.continue_proxy_set_prototype_invariant(continuation.site(), mode, state, value)
            }
            ProxySetPrototypeStage::TargetGetPrototypeOf => {
                let pending = self.native_call_state_snapshot(state)?;
                if value != pending.values[PROXY_PROTOTYPE_ARGUMENT] {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                self.finish_proxy_set_prototype(
                    continuation.site(),
                    mode,
                    pending.values[PROXY_RESULT_OBJECT],
                    true,
                )
            }
        }
    }

    /// Applies GetMethod nullish/callable rules before invoking `(target, prototype)`.
    fn continue_proxy_set_prototype_lookup(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetPrototypeMode,
        state: GcRef<NativeCallState>,
        trap: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        if matches!(
            trap.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            let target = pending.values[PROXY_TARGET_ARGUMENT];
            if self.is_proxy_value(target) {
                return self.dispatch_proxy_set_prototype(
                    site,
                    target,
                    pending.values[PROXY_PROTOTYPE_ARGUMENT],
                    pending.values[PROXY_RESULT_OBJECT],
                    mode,
                );
            }
            let success =
                self.ordinary_set_prototype_of(target, pending.values[PROXY_PROTOTYPE_ARGUMENT])?;
            return self.finish_proxy_set_prototype(
                site,
                mode,
                pending.values[PROXY_RESULT_OBJECT],
                success,
            );
        }
        self.resolve_function_object(trap)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_set_prototype(
                site,
                mode,
                ProxySetPrototypeStage::TrapCall,
                Value::from_heap_ref(state.raw()),
                trap,
            ),
            trap,
        )
    }

    /// Converts the trap result and starts the target's observable extensibility check when needed.
    fn finish_proxy_set_prototype_trap(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetPrototypeMode,
        state: GcRef<NativeCallState>,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if !self.is_truthy_value(trap_result)? {
            let pending = self.native_call_state_snapshot(state)?;
            return self.finish_proxy_set_prototype(
                site,
                mode,
                pending.values[PROXY_RESULT_OBJECT],
                false,
            );
        }
        let pending = self.native_call_state_snapshot(state)?;
        let target = pending.values[PROXY_TARGET_ARGUMENT];
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_set_prototype_target_method(
                site,
                mode,
                state,
                ProxySetPrototypeStage::TargetIsExtensible,
                target,
                ProxyInternalMethod::IsExtensible,
            );
        }
        let extensible = self.object_snapshot(target)?.1.extensible;
        self.continue_proxy_set_prototype_invariant(site, mode, state, boolean_value(extensible))
    }

    /// Finishes on extensible targets or obtains the target prototype for the final invariant.
    fn continue_proxy_set_prototype_invariant(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetPrototypeMode,
        state: GcRef<NativeCallState>,
        extensible: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        if self.is_truthy_value(extensible)? {
            return self.finish_proxy_set_prototype(
                site,
                mode,
                pending.values[PROXY_RESULT_OBJECT],
                true,
            );
        }
        let target = pending.values[PROXY_TARGET_ARGUMENT];
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_set_prototype_target_method(
                site,
                mode,
                state,
                ProxySetPrototypeStage::TargetGetPrototypeOf,
                target,
                ProxyInternalMethod::GetPrototypeOf,
            );
        }
        let prototype = self.object_snapshot(target)?.1.prototype;
        if prototype != pending.values[PROXY_PROTOTYPE_ARGUMENT] {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        self.finish_proxy_set_prototype(site, mode, pending.values[PROXY_RESULT_OBJECT], true)
    }

    /// Publishes one parent continuation around a possibly suspended target internal method.
    fn dispatch_proxy_set_prototype_target_method(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetPrototypeMode,
        state: GcRef<NativeCallState>,
        stage: ProxySetPrototypeStage,
        target: Value,
        operation: ProxyInternalMethod,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_set_prototype(
                site,
                mode,
                stage,
                Value::from_heap_ref(state.raw()),
                target,
            ))
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })?;
        let frame_depth = self.fiber.frames.len();
        let outcome = match self.dispatch_proxy_internal_method(site, target, operation) {
            Ok(outcome) => outcome,
            Err(error) => {
                if self.fiber.completions.len() > completion_depth {
                    self.pop_native_continuation()?;
                }
                return Err(error);
            }
        };
        if self.fiber.completions.len() == completion_depth
            || self.fiber.frames.len() != frame_depth
        {
            return Ok(outcome);
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_set_prototype(continuation, mode, stage, value)
    }

    /// Maps the internal boolean result to Reflect or Object.setPrototypeOf's public contract.
    fn finish_proxy_set_prototype(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetPrototypeMode,
        result_object: Value,
        success: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let result = match mode {
            ProxySetPrototypeMode::Reflect => boolean_value(success),
            ProxySetPrototypeMode::Object if success => result_object,
            ProxySetPrototypeMode::Object => {
                return Err(ExecutionError::ProxyInvariantViolation);
            }
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }

    /// Allocates the traced `(target, prototype)` argument source plus invariant identities.
    fn allocate_proxy_set_prototype_state(
        &mut self,
        target: Value,
        prototype: Value,
        active_proxy: Value,
        result_object: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let values = [target, prototype, active_proxy, result_object, undefined];
        let mut roots = NativeCallStateRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            values,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                NativeCallState {
                    values: roots.values,
                    count: 2,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn native_call_state_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.native_call_state)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    #[inline(always)]
    pub(crate) fn is_proxy_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.proxy_object)
                .is_ok()
        })
    }

    /// Copies the two traced Proxy slots through a checked no-GC borrow.
    pub(crate) fn proxy_snapshot(&mut self, value: Value) -> Result<ProxyObject, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let proxy = self
            .heap
            .checked_reference(raw, self.types.proxy_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let proxy = scope.root(proxy).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(proxy, self.types.proxy_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Starts GetMethod(handler, trapName), suspending if the trap property is an accessor.
    pub(crate) fn dispatch_proxy_internal_method(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        operation: ProxyInternalMethod,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(operation.trap_name())?;
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target)
                        && operation != ProxyInternalMethod::PreventExtensionsObject
                    {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_internal_method(
                        site,
                        proxy,
                        snapshot.target,
                        operation,
                    );
                }
                PropertyRead::Data(trap) => {
                    return self.continue_proxy_trap_lookup(site, proxy, operation, trap);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target)
                        && operation != ProxyInternalMethod::PreventExtensionsObject
                    {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_internal_method(
                        site,
                        proxy,
                        snapshot.target,
                        operation,
                    );
                }
                PropertyRead::Accessor(getter) => {
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_trap_getter(
                            site,
                            operation,
                            proxy,
                            snapshot.handler,
                        ),
                        getter,
                    );
                }
            }
        }
    }

    /// Resumes either the trap-property getter or the actual trap call.
    pub(crate) fn resume_proxy_internal_method(
        &mut self,
        continuation: NativeContinuation,
        operation: ProxyInternalMethod,
        stage: ProxyContinuationStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match stage {
            ProxyContinuationStage::TrapGetter => self.continue_proxy_trap_lookup(
                continuation.site(),
                continuation.first(),
                operation,
                value,
            ),
            ProxyContinuationStage::TrapCall => self.finish_proxy_trap_call(
                continuation.site(),
                continuation.first(),
                operation,
                value,
            ),
            ProxyContinuationStage::ForwardResult => {
                self.finish_proxy_forward_result(continuation.site(), continuation.first(), value)
            }
        }
    }

    /// Applies GetMethod's nullish/callable rules and invokes one trap with the target argument.
    fn continue_proxy_trap_lookup(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        operation: ProxyInternalMethod,
        trap: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if matches!(
            trap.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            let target = self.proxy_snapshot(proxy)?.target;
            return self.forward_proxy_internal_method(site, proxy, target, operation);
        }
        self.resolve_function_object(trap)?;
        let target = self.proxy_snapshot(proxy)?.target;
        self.write(site.caller_base, site.destination, target)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_trap_call(site, operation, proxy, trap),
            trap,
        )
    }

    /// Executes the ordinary target operation used when the corresponding Proxy trap is absent.
    fn forward_proxy_internal_method(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        target: Value,
        operation: ProxyInternalMethod,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if self.is_proxy_value(target) {
            return if operation == ProxyInternalMethod::PreventExtensionsObject {
                self.forward_object_prevent_extensions(site, proxy, target)
            } else {
                self.dispatch_proxy_internal_method(site, target, operation)
            };
        }
        let (receiver, snapshot) = self.object_snapshot(target)?;
        let result = match operation {
            ProxyInternalMethod::GetPrototypeOf => snapshot.prototype,
            ProxyInternalMethod::IsExtensible => boolean_value(snapshot.extensible),
            ProxyInternalMethod::PreventExtensions => {
                self.set_object_extensible(receiver, false)?;
                boolean_value(true)
            }
            ProxyInternalMethod::PreventExtensionsObject => {
                self.set_object_extensible(receiver, false)?;
                proxy
            }
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }

    /// Maps a nested Proxy's boolean [[PreventExtensions]] result back to Object.preventExtensions.
    fn forward_object_prevent_extensions(
        &mut self,
        site: NativeContinuationSite,
        outer_proxy: Value,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_forward_result(site, outer_proxy))
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })?;
        let frame_depth = self.fiber.frames.len();
        let result = self.dispatch_proxy_internal_method(
            site,
            target,
            ProxyInternalMethod::PreventExtensions,
        );
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                if self.fiber.completions.len() > completion_depth {
                    self.pop_native_continuation()?;
                }
                return Err(error);
            }
        };
        if self.fiber.completions.len() == completion_depth {
            return Ok(outcome);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(outcome);
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.finish_proxy_forward_result(site, continuation.first(), value)
    }

    /// Applies Object.preventExtensions' throw-on-false and original-object return contract.
    fn finish_proxy_forward_result(
        &mut self,
        site: NativeContinuationSite,
        outer_proxy: Value,
        result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if !self.is_truthy_value(result)? {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        self.write(site.caller_base, site.destination, outer_proxy)?;
        Ok(None)
    }

    /// Enforces the target invariant after a trap's normal completion.
    fn finish_proxy_trap_call(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        operation: ProxyInternalMethod,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let target = self.proxy_snapshot(proxy)?.target;
        let (_, target_snapshot) = self.object_snapshot(target)?;
        let result = match operation {
            ProxyInternalMethod::GetPrototypeOf => {
                if trap_result.as_immediate() != Some(Immediate::Null)
                    && !self.is_object_value(trap_result)
                {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                if !target_snapshot.extensible && trap_result != target_snapshot.prototype {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                trap_result
            }
            ProxyInternalMethod::IsExtensible => {
                let result = self.is_truthy_value(trap_result)?;
                if result != target_snapshot.extensible {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                boolean_value(result)
            }
            ProxyInternalMethod::PreventExtensions => {
                let result = self.is_truthy_value(trap_result)?;
                if result && target_snapshot.extensible {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                boolean_value(result)
            }
            ProxyInternalMethod::PreventExtensionsObject => {
                let result = self.is_truthy_value(trap_result)?;
                if !result || target_snapshot.extensible {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                proxy
            }
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }
}

impl ProxyInternalMethod {
    #[inline(always)]
    const fn trap_name(self) -> &'static [u8] {
        match self {
            Self::GetPrototypeOf => b"getPrototypeOf",
            Self::IsExtensible => b"isExtensible",
            Self::PreventExtensions | Self::PreventExtensionsObject => b"preventExtensions",
        }
    }
}
