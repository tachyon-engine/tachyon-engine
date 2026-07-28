//! Proxy `[[HasProperty]]` dispatch, continuations, and invariants.

use super::*;

impl Isolate {
    /// Walks ordinary prototypes until a Proxy exotic owns the remaining `[[HasProperty]]` query.
    pub(crate) fn dispatch_has_property(
        &mut self,
        site: NativeContinuationSite,
        mut receiver: Value,
        key_value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let key = self.property_key(key_value)?;
        let trap_key = match key {
            PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
            PropertyKey::Symbol(symbol) => symbol.value(),
            PropertyKey::Private(_) => return Err(ExecutionError::PrivatePropertyKeyEscaped),
        };
        loop {
            if self.is_proxy_value(receiver) {
                return self.dispatch_proxy_has(site, receiver, trap_key);
            }
            if self.is_regexp_value(receiver) && self.regexp_virtual_property(key)? {
                self.write(site.caller_base, site.destination, boolean_value(true))?;
                return Ok(None);
            }
            if self
                .complete_own_property_descriptor(receiver, key)?
                .is_some()
            {
                self.write(site.caller_base, site.destination, boolean_value(true))?;
                return Ok(None);
            }
            let prototype = self.object_snapshot(receiver)?.1.prototype;
            if prototype.as_immediate() == Some(Immediate::Null) {
                self.write(site.caller_base, site.destination, boolean_value(false))?;
                return Ok(None);
            }
            if !self.is_object_value(prototype) {
                return Err(ExecutionError::NotObject(prototype));
            }
            receiver = prototype;
        }
    }

    /// Executes Proxy `[[HasProperty]]`, iteratively forwarding missing traps through nested proxies.
    pub(crate) fn dispatch_proxy_has(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        key: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"has")?;
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.finish_proxy_has_forward(site, snapshot.target, key);
                }
                PropertyRead::Data(trap) => {
                    let state = self.allocate_proxy_has_state(snapshot.target, key, proxy)?;
                    return self.continue_proxy_has_lookup(site, state, trap);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.finish_proxy_has_forward(site, snapshot.target, key);
                }
                PropertyRead::Accessor(getter) => {
                    let state = self.allocate_proxy_has_state(snapshot.target, key, proxy)?;
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_has(
                            site,
                            ProxyHasStage::TrapGetter,
                            Value::from_heap_ref(state.raw()),
                            snapshot.handler,
                        ),
                        getter,
                    );
                }
            }
        }
    }

    /// Resumes either the accessor-backed trap lookup or the actual `(target, key)` trap call.
    pub(crate) fn resume_proxy_has(
        &mut self,
        continuation: NativeContinuation,
        stage: ProxyHasStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ProxyHasStage::TrapGetter => {
                self.continue_proxy_has_lookup(continuation.site(), state, value)
            }
            ProxyHasStage::TrapCall => {
                self.finish_proxy_has_trap(continuation.site(), state, value)
            }
            ProxyHasStage::TargetGetOwn => {
                self.continue_proxy_has_target_descriptor(continuation.site(), state, value)
            }
            ProxyHasStage::TargetIsExtensible => {
                if !self.is_truthy_value(value)? {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                self.write(
                    continuation.site().caller_base,
                    continuation.site().destination,
                    boolean_value(false),
                )?;
                Ok(None)
            }
        }
    }

    /// Applies GetMethod semantics and invokes a callable trap with the handler as `this`.
    fn continue_proxy_has_lookup(
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
            let target = pending.values[PROXY_TARGET_ARGUMENT];
            if self.is_proxy_value(target) {
                return self.dispatch_proxy_has(
                    site,
                    target,
                    pending.values[PROXY_HAS_KEY_ARGUMENT],
                );
            }
            return self.finish_proxy_has_forward(
                site,
                target,
                pending.values[PROXY_HAS_KEY_ARGUMENT],
            );
        }
        self.resolve_function_object(trap)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_has(
                site,
                ProxyHasStage::TrapCall,
                Value::from_heap_ref(state.raw()),
                trap,
            ),
            trap,
        )
    }

    /// Converts the trap result and enforces the false-result non-configurable/non-extensible invariant.
    fn finish_proxy_has_trap(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let result = self.is_truthy_value(trap_result)?;
        if !result {
            let pending = self.native_call_state_snapshot(state)?;
            let target = pending.values[PROXY_TARGET_ARGUMENT];
            if self.is_proxy_value(target) {
                return self.dispatch_proxy_has_target_get_own(site, state, target);
            }
            let key = self.property_key(pending.values[PROXY_HAS_KEY_ARGUMENT])?;
            if let Some(descriptor) = self.complete_own_property_descriptor(target, key)? {
                let configurable = match descriptor {
                    PropertyDescriptor::Generic(descriptor) => descriptor.configurable,
                    PropertyDescriptor::Data(descriptor) => descriptor.configurable,
                    PropertyDescriptor::Accessor(descriptor) => descriptor.configurable,
                }
                .unwrap_or(false);
                let extensible = self.object_snapshot(target)?.1.extensible;
                if !configurable || !extensible {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
            }
        }
        self.write(site.caller_base, site.destination, boolean_value(result))?;
        Ok(None)
    }

    /// Resumes the nested target descriptor check before the target extensibility invariant.
    fn continue_proxy_has_target_descriptor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        descriptor: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if descriptor.as_immediate() == Some(Immediate::Undefined) {
            self.write(site.caller_base, site.destination, boolean_value(false))?;
            return Ok(None);
        }
        let descriptor = self.parse_property_descriptor(descriptor)?;
        if descriptor.configurable() == Some(false) {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        let target = self.native_call_state_snapshot(state)?.values[PROXY_TARGET_ARGUMENT];
        self.dispatch_proxy_has_target_extensible(site, state, target)
    }

    fn dispatch_proxy_has_target_get_own(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_has_parent(site, state, ProxyHasStage::TargetGetOwn, target)?;
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
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_has(continuation, ProxyHasStage::TargetGetOwn, value)
    }

    fn dispatch_proxy_has_target_extensible(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_has_parent(site, state, ProxyHasStage::TargetIsExtensible, target)?;
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
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_has(continuation, ProxyHasStage::TargetIsExtensible, value)
    }

    fn push_proxy_has_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ProxyHasStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_has(
                site,
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

    /// Finishes a missing/nullish trap by applying the ordinary target's prototype-chain lookup.
    fn finish_proxy_has_forward(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.dispatch_has_property(site, target, key)
    }

    /// Allocates the fixed `(target, key)` argument source and retains the active Proxy identity.
    fn allocate_proxy_has_state(
        &mut self,
        target: Value,
        key: Value,
        active_proxy: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let values = [target, key, active_proxy, undefined, undefined];
        let mut roots = NativeCallStateRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
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
