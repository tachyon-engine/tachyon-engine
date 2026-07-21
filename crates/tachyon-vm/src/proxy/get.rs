//! Proxy `[[Get]]` dispatch, receiver preservation, and descriptor invariants.

use super::*;

const PROXY_GET_RECEIVER: usize = 2;
pub(crate) const PROXY_GET_ACTIVE: usize = 3;
const PROXY_GET_TRAP_RESULT: usize = 4;

impl Isolate {
    /// Shares Proxy-aware property reads between bytecode and Reflect.get while retaining receiver.
    pub(crate) fn dispatch_proxy_aware_property_read(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match self.resolve_property_read_until_proxy(target, key)? {
            PropertyReadResolution::Read(read) => {
                self.finish_proxy_aware_ordinary_read(site, receiver, read)
            }
            PropertyReadResolution::Proxy(proxy) => {
                let key = match key {
                    PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
                    PropertyKey::Symbol(symbol) => symbol.value(),
                };
                self.dispatch_proxy_get(site, proxy, key, receiver)
            }
        }
    }

    /// Completes the ordinary branch without allocating descriptor or Proxy continuation state.
    fn finish_proxy_aware_ordinary_read(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        read: PropertyRead,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match read {
            PropertyRead::Missing => self
                .write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                )
                .map(|()| None),
            PropertyRead::Data(value) => self
                .write(site.caller_base, site.destination, value)
                .map(|()| None),
            PropertyRead::Accessor(getter)
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                )?;
                Ok(None)
            }
            PropertyRead::Accessor(getter) => self.dispatch_property_callback(
                NativeContinuation::property_get(site, PropertyCallbackMode::Ordinary, receiver),
                getter,
            ),
        }
    }

    /// Executes Proxy `[[Get]]`, forwarding nullish traps through nested targets.
    fn dispatch_proxy_get(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        key: Value,
        receiver: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"get")?;
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_get(site, snapshot.target, key, receiver);
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
                        return self.forward_proxy_get(site, snapshot.target, key, receiver);
                    }
                    let state =
                        self.allocate_proxy_get_state(snapshot.target, key, receiver, proxy, trap)?;
                    let trap =
                        self.native_call_state_snapshot(state)?.values[PROXY_GET_TRAP_RESULT];
                    return self.continue_proxy_get_lookup(site, state, trap);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_get(site, snapshot.target, key, receiver);
                }
                PropertyRead::Accessor(getter) => {
                    let state = self.allocate_proxy_get_state(
                        snapshot.target,
                        key,
                        receiver,
                        proxy,
                        getter,
                    )?;
                    let pending = self.native_call_state_snapshot(state)?;
                    let getter = pending.values[PROXY_GET_TRAP_RESULT];
                    let handler = self
                        .proxy_snapshot(pending.values[PROXY_GET_ACTIVE])?
                        .handler;
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_get(
                            site,
                            ProxyGetStage::TrapGetter,
                            Value::from_heap_ref(state.raw()),
                            handler,
                        ),
                        getter,
                    );
                }
            }
        }
    }

    pub(crate) fn resume_proxy_get(
        &mut self,
        continuation: NativeContinuation,
        stage: ProxyGetStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ProxyGetStage::TrapGetter => {
                self.continue_proxy_get_lookup(continuation.site(), state, value)
            }
            ProxyGetStage::TrapCall => {
                self.finish_proxy_get_trap(continuation.site(), state, value)
            }
            ProxyGetStage::TargetGetOwn => {
                self.finish_proxy_get_target_descriptor(continuation.site(), state, value)
            }
        }
    }

    fn continue_proxy_get_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        trap: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        if matches!(
            trap.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return self.forward_proxy_get(
                site,
                pending.values[PROXY_TARGET_ARGUMENT],
                pending.values[PROXY_HAS_KEY_ARGUMENT],
                pending.values[PROXY_GET_RECEIVER],
            );
        }
        self.resolve_function_object(trap)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_get(
                site,
                ProxyGetStage::TrapCall,
                Value::from_heap_ref(state.raw()),
                trap,
            ),
            trap,
        )
    }

    fn forward_proxy_get(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: Value,
        receiver: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let key_identity = self.property_key(key)?;
        self.dispatch_proxy_aware_property_read(site, target, receiver, key_identity)
    }

    /// Checks the target's own descriptor after the trap result without changing the result value.
    fn finish_proxy_get_trap(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.update_proxy_state_value(state, PROXY_GET_TRAP_RESULT, trap_result)?;
        let pending = self.native_call_state_snapshot(state)?;
        let target = pending.values[PROXY_TARGET_ARGUMENT];
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_get_target_descriptor(site, state, target);
        }
        let key = self.property_key(pending.values[PROXY_HAS_KEY_ARGUMENT])?;
        let descriptor = self.complete_own_property_descriptor(target, key)?;
        let trap_result = self.native_call_state_snapshot(state)?.values[PROXY_GET_TRAP_RESULT];
        self.validate_proxy_get_result(trap_result, descriptor)?;
        self.write(site.caller_base, site.destination, trap_result)?;
        Ok(None)
    }

    fn finish_proxy_get_target_descriptor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        descriptor: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let descriptor = if descriptor.as_immediate() == Some(Immediate::Undefined) {
            None
        } else {
            Some(self.parse_property_descriptor(descriptor)?)
        };
        let trap_result = self.native_call_state_snapshot(state)?.values[PROXY_GET_TRAP_RESULT];
        self.validate_proxy_get_result(trap_result, descriptor)?;
        self.write(site.caller_base, site.destination, trap_result)?;
        Ok(None)
    }

    /// Enforces the two result restrictions for frozen data and getter-less accessor properties.
    fn validate_proxy_get_result(
        &mut self,
        trap_result: Value,
        descriptor: Option<PropertyDescriptor>,
    ) -> Result<(), ExecutionError> {
        let Some(descriptor) = descriptor else {
            return Ok(());
        };
        match descriptor {
            PropertyDescriptor::Data(descriptor)
                if descriptor.configurable == Some(false) && descriptor.writable == Some(false) =>
            {
                if !self.same_value(
                    trap_result,
                    descriptor
                        .value
                        .unwrap_or(Value::from_immediate(Immediate::Undefined)),
                )? {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
            }
            PropertyDescriptor::Accessor(descriptor)
                if descriptor.configurable == Some(false)
                    && descriptor.getter.is_none_or(|getter| {
                        getter.as_immediate() == Some(Immediate::Undefined)
                    }) =>
            {
                if trap_result.as_immediate() != Some(Immediate::Undefined) {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
            }
            PropertyDescriptor::Generic(_)
            | PropertyDescriptor::Data(_)
            | PropertyDescriptor::Accessor(_) => {}
        }
        Ok(())
    }

    /// Suspends the outer invariant check while a nested target resolves its own descriptor.
    fn dispatch_proxy_get_target_descriptor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_get(
                site,
                ProxyGetStage::TargetGetOwn,
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
        self.resume_proxy_get(continuation, ProxyGetStage::TargetGetOwn, descriptor)
    }

    /// Allocates the fixed Proxy get state, rooting the callee until its callback is published.
    fn allocate_proxy_get_state(
        &mut self,
        target: Value,
        key: Value,
        receiver: Value,
        active_proxy: Value,
        retained_callee: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let values = [target, key, receiver, active_proxy, retained_callee];
        let mut roots = NativeCallStateRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
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
                    count: 3,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }
}
