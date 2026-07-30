//! Proxy `[[Delete]]` dispatch, public result modes, and target invariants.

use super::*;

pub(crate) const PROXY_DELETE_ACTIVE: usize = 2;
const PROXY_DELETE_RETAINED: usize = 3;

impl Isolate {
    /// Keeps opcode ordinary deletion on `PropertyKey` after its Proxy branch is selected.
    pub(crate) fn finish_ordinary_delete_property_key(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: PropertyKey,
        mode: ProxyDeleteMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        debug_assert!(!self.is_proxy_value(target));
        let deleted = self.delete_own_data_property(target, key)?;
        self.finish_proxy_delete_mode(site, mode, target, deleted)
    }

    /// Routes ordinary and Proxy deletion through one mode-aware internal-method boundary.
    pub(crate) fn dispatch_delete_property(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: Value,
        mode: ProxyDeleteMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_delete(site, target, key, mode);
        }
        let key_identity = self.property_key(key)?;
        let deleted = self.delete_own_data_property(target, key_identity)?;
        self.finish_proxy_delete_mode(site, mode, target, deleted)
    }

    /// Executes Proxy `[[Delete]]`, iterating through synchronous missing-trap chains.
    fn dispatch_proxy_delete(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        key: Value,
        mode: ProxyDeleteMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"deleteProperty")?;
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_delete(site, snapshot.target, key, mode);
                }
                PropertyRead::Data(trap) => {
                    if matches!(
                        trap.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) {
                        if self.is_proxy_value(snapshot.target) {
                            proxy = snapshot.target;
                            continue;
                        }
                        return self.forward_proxy_delete(site, snapshot.target, key, mode);
                    }
                    let state =
                        self.allocate_proxy_delete_state(snapshot.target, key, proxy, trap)?;
                    let trap =
                        self.native_call_state_snapshot(state)?.values[PROXY_DELETE_RETAINED];
                    return self.continue_proxy_delete_lookup(site, mode, state, trap);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_delete(site, snapshot.target, key, mode);
                }
                PropertyRead::Accessor(getter) => {
                    let state =
                        self.allocate_proxy_delete_state(snapshot.target, key, proxy, getter)?;
                    let pending = self.native_call_state_snapshot(state)?;
                    let getter = pending.values[PROXY_DELETE_RETAINED];
                    let handler = self
                        .proxy_snapshot(pending.values[PROXY_DELETE_ACTIVE])?
                        .handler;
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_delete(
                            site,
                            mode,
                            ProxyDeleteStage::TrapGetter,
                            Value::from_heap_ref(state.raw()),
                            handler,
                        ),
                        getter,
                    );
                }
            }
        }
    }

    /// Resumes trap lookup/call and both observable target invariant operations.
    pub(crate) fn resume_proxy_delete(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxyDeleteMode,
        stage: ProxyDeleteStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ProxyDeleteStage::TrapGetter => {
                self.continue_proxy_delete_lookup(continuation.site(), mode, state, value)
            }
            ProxyDeleteStage::TrapCall => {
                self.finish_proxy_delete_trap(continuation.site(), mode, state, value)
            }
            ProxyDeleteStage::TargetGetOwn => {
                self.finish_proxy_delete_target_descriptor(continuation.site(), mode, state, value)
            }
            ProxyDeleteStage::TargetIsExtensible => {
                if !self.is_truthy_value(value)? {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                let proxy = self.native_call_state_snapshot(state)?.values[PROXY_DELETE_ACTIVE];
                self.finish_proxy_delete_mode(continuation.site(), mode, proxy, true)
            }
        }
    }

    /// Applies GetMethod nullish/callable rules and invokes `(target, key)` with handler `this`.
    fn continue_proxy_delete_lookup(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
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
                return self.dispatch_proxy_delete(
                    site,
                    target,
                    pending.values[PROXY_HAS_KEY_ARGUMENT],
                    mode,
                );
            }
            return self.forward_proxy_delete(
                site,
                target,
                pending.values[PROXY_HAS_KEY_ARGUMENT],
                mode,
            );
        }
        self.resolve_function_object(trap)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_delete(
                site,
                mode,
                ProxyDeleteStage::TrapCall,
                Value::from_heap_ref(state.raw()),
                trap,
            ),
            trap,
        )
    }

    /// Converts the trap result and starts the target descriptor invariant only on true.
    fn finish_proxy_delete_trap(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        state: GcRef<NativeCallState>,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if !self.is_truthy_value(trap_result)? {
            let proxy = self.native_call_state_snapshot(state)?.values[PROXY_DELETE_ACTIVE];
            return self.finish_proxy_delete_mode(site, mode, proxy, false);
        }
        let pending = self.native_call_state_snapshot(state)?;
        let target = pending.values[PROXY_TARGET_ARGUMENT];
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_delete_target_get_own(site, mode, state, target);
        }
        let key = self.property_key(pending.values[PROXY_HAS_KEY_ARGUMENT])?;
        let descriptor = self.complete_own_property_descriptor(target, key)?;
        self.continue_proxy_delete_descriptor(site, mode, state, descriptor)
    }

    /// Parses a nested target descriptor before applying configurable/extensible restrictions.
    fn finish_proxy_delete_target_descriptor(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        state: GcRef<NativeCallState>,
        descriptor: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let descriptor = if descriptor.as_immediate() == Some(Immediate::Undefined) {
            None
        } else {
            Some(self.parse_property_descriptor(descriptor)?)
        };
        self.continue_proxy_delete_descriptor(site, mode, state, descriptor)
    }

    /// Rejects hidden non-configurable or non-extensible properties after a true trap result.
    fn continue_proxy_delete_descriptor(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        state: GcRef<NativeCallState>,
        descriptor: Option<PropertyDescriptor>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let proxy = self.native_call_state_snapshot(state)?.values[PROXY_DELETE_ACTIVE];
        let Some(descriptor) = descriptor else {
            return self.finish_proxy_delete_mode(site, mode, proxy, true);
        };
        if descriptor.configurable() == Some(false) {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        let target = self.native_call_state_snapshot(state)?.values[PROXY_TARGET_ARGUMENT];
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_delete_target_extensible(site, mode, state, target);
        }
        if !self.object_snapshot(target)?.1.extensible {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        self.finish_proxy_delete_mode(site, mode, proxy, true)
    }

    /// Suspends the outer delete invariant while a Proxy target resolves its own descriptor.
    fn dispatch_proxy_delete_target_get_own(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_delete_parent(site, mode, state, ProxyDeleteStage::TargetGetOwn, target)?;
        let key = self.native_call_state_snapshot(state)?.values[PROXY_HAS_KEY_ARGUMENT];
        let outcome =
            match self.dispatch_proxy_get_own(site, target, key, ProxyGetOwnMode::Descriptor) {
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
        let descriptor = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_delete(
            continuation,
            mode,
            ProxyDeleteStage::TargetGetOwn,
            descriptor,
        )
    }

    /// Suspends the outer delete invariant while a Proxy target reports extensibility.
    fn dispatch_proxy_delete_target_extensible(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_delete_parent(
            site,
            mode,
            state,
            ProxyDeleteStage::TargetIsExtensible,
            target,
        )?;
        let outcome = match self.dispatch_proxy_internal_method(
            site,
            target,
            ProxyInternalMethod::IsExtensible,
        ) {
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
        let extensible = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_delete(
            continuation,
            mode,
            ProxyDeleteStage::TargetIsExtensible,
            extensible,
        )
    }

    /// Pushes one traced parent continuation for a nested target internal method.
    fn push_proxy_delete_parent(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        state: GcRef<NativeCallState>,
        stage: ProxyDeleteStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_delete(
                site,
                mode,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })
    }

    /// Forwards an absent trap to one ordinary target without allocating continuation state.
    fn forward_proxy_delete(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: Value,
        mode: ProxyDeleteMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let key = self.property_key(key)?;
        let deleted = self.delete_own_data_property(target, key)?;
        self.finish_proxy_delete_mode(site, mode, target, deleted)
    }

    /// Maps the internal boolean result to Reflect or DeletePropertyOrThrow behavior.
    fn finish_proxy_delete_mode(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        subject: Value,
        success: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if !success && mode == ProxyDeleteMode::Strict {
            return Err(ExecutionError::ReadOnlyProperty(subject));
        }
        self.write(site.caller_base, site.destination, boolean_value(success))?;
        Ok(None)
    }

    /// Allocates the fixed `(target, key)` source and roots a callee across its safepoint.
    fn allocate_proxy_delete_state(
        &mut self,
        target: Value,
        key: Value,
        active_proxy: Value,
        retained_callee: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let values = [target, key, active_proxy, retained_callee, undefined];
        let mut roots = NativeCallStateRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
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
}
