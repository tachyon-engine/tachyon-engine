//! Proxy `[[GetOwnProperty]]` dispatch, continuations, and invariants.

use super::*;

impl Isolate {
    /// Starts Proxy `[[GetOwnProperty]]` and forwards missing traps through nested targets.
    pub(crate) fn dispatch_proxy_get_own(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        key: Value,
        mode: ProxyGetOwnMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"getOwnPropertyDescriptor")?;
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    let key_identity = self.property_key(key)?;
                    let descriptor =
                        self.complete_own_property_descriptor(snapshot.target, key_identity)?;
                    return self.finish_proxy_get_own_mode(site, mode, descriptor);
                }
                PropertyRead::Data(trap) => {
                    let state = self.allocate_proxy_get_own_state(snapshot.target, key, proxy)?;
                    return self.continue_proxy_get_own_lookup(site, mode, state, trap);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    let key_identity = self.property_key(key)?;
                    let descriptor =
                        self.complete_own_property_descriptor(snapshot.target, key_identity)?;
                    return self.finish_proxy_get_own_mode(site, mode, descriptor);
                }
                PropertyRead::Accessor(getter) => {
                    let state = self.allocate_proxy_get_own_state(snapshot.target, key, proxy)?;
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_get_own(
                            site,
                            mode,
                            ProxyGetOwnStage::TrapGetter,
                            Value::from_heap_ref(state.raw()),
                            snapshot.handler,
                        ),
                        getter,
                    );
                }
            }
        }
    }

    pub(crate) fn resume_proxy_get_own(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxyGetOwnMode,
        stage: ProxyGetOwnStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ProxyGetOwnStage::TrapGetter => {
                self.continue_proxy_get_own_lookup(continuation.site(), mode, state, value)
            }
            ProxyGetOwnStage::TrapCall => {
                self.finish_proxy_get_own_trap(continuation.site(), mode, state, value)
            }
            ProxyGetOwnStage::TargetGetOwn => self.continue_proxy_get_own_target_descriptor(
                continuation.site(),
                mode,
                state,
                value,
            ),
            ProxyGetOwnStage::TargetIsExtensible => self.continue_proxy_get_own_target_extensible(
                continuation.site(),
                mode,
                state,
                value,
            ),
        }
    }

    fn continue_proxy_get_own_lookup(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
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
                return self.dispatch_proxy_get_own(
                    site,
                    target,
                    pending.values[PROXY_HAS_KEY_ARGUMENT],
                    mode,
                );
            }
            let key = self.property_key(pending.values[PROXY_HAS_KEY_ARGUMENT])?;
            let descriptor = self.complete_own_property_descriptor(target, key)?;
            return self.finish_proxy_get_own_mode(site, mode, descriptor);
        }
        self.resolve_function_object(trap)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_get_own(
                site,
                mode,
                ProxyGetOwnStage::TrapCall,
                Value::from_heap_ref(state.raw()),
                trap,
            ),
            trap,
        )
    }

    /// Performs target descriptor/extensibility checks before any trap descriptor getter executes.
    fn finish_proxy_get_own_trap(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if trap_result.as_immediate() != Some(Immediate::Undefined)
            && !self.is_object_value(trap_result)
        {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        self.update_proxy_state_value(state, PROXY_GET_OWN_DESCRIPTOR, trap_result)?;
        let pending = self.native_call_state_snapshot(state)?;
        let target = pending.values[PROXY_TARGET_ARGUMENT];
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_get_own_target_descriptor(site, mode, state, target);
        }
        let key = self.property_key(pending.values[PROXY_HAS_KEY_ARGUMENT])?;
        let target_descriptor = self.complete_own_property_descriptor(target, key)?;
        if trap_result.as_immediate() == Some(Immediate::Undefined) && target_descriptor.is_none() {
            return self.finish_proxy_get_own_mode(site, mode, None);
        }
        let extensible = self.object_snapshot(target)?.1.extensible;
        self.continue_proxy_get_own_after_target(
            site,
            mode,
            state,
            trap_result,
            target_descriptor,
            extensible,
        )
    }

    fn continue_proxy_get_own_after_target(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        trap_result: Value,
        target_descriptor: Option<PropertyDescriptor>,
        extensible: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        if trap_result.as_immediate() == Some(Immediate::Undefined) {
            if let Some(target) = target_descriptor
                && (target.configurable() == Some(false) || !extensible)
            {
                return Err(ExecutionError::ProxyInvariantViolation);
            }
            return self.finish_proxy_get_own_mode(site, mode, None);
        }
        let target_descriptor_object = if let Some(descriptor) = target_descriptor {
            let object = self.create_ordinary_object()?;
            self.materialize_property_descriptor(object, descriptor)?;
            object
        } else {
            Value::from_immediate(Immediate::Undefined)
        };
        self.update_proxy_state_value(state, PROXY_ACTIVE_OBJECT, boolean_value(extensible))?;
        self.update_proxy_state_value(
            state,
            PROXY_GET_OWN_TARGET_DESCRIPTOR,
            target_descriptor_object,
        )?;
        let key = self.property_key(pending.values[PROXY_HAS_KEY_ARGUMENT])?;
        self.begin_proxy_property_descriptor(site, state, mode, key, trap_result)?;
        Ok(None)
    }

    /// Stores a nested target descriptor and conditionally starts the observable extensibility check.
    fn continue_proxy_get_own_target_descriptor(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        descriptor: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.update_proxy_state_value(state, PROXY_GET_OWN_TARGET_DESCRIPTOR, descriptor)?;
        let pending = self.native_call_state_snapshot(state)?;
        let trap_result = pending.values[PROXY_GET_OWN_DESCRIPTOR];
        if descriptor.as_immediate() == Some(Immediate::Undefined)
            && trap_result.as_immediate() == Some(Immediate::Undefined)
        {
            return self.finish_proxy_get_own_mode(site, mode, None);
        }
        self.dispatch_proxy_get_own_target_extensible(
            site,
            mode,
            state,
            pending.values[PROXY_TARGET_ARGUMENT],
        )
    }

    /// Resumes nested `[[IsExtensible]]` and only then begins trap descriptor field observation.
    fn continue_proxy_get_own_target_extensible(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        extensible: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.update_proxy_state_value(state, PROXY_ACTIVE_OBJECT, extensible)?;
        let pending = self.native_call_state_snapshot(state)?;
        let descriptor = pending.values[PROXY_GET_OWN_TARGET_DESCRIPTOR];
        let target_descriptor = if descriptor.as_immediate() == Some(Immediate::Undefined) {
            None
        } else {
            Some(self.parse_property_descriptor(descriptor)?)
        };
        let extensible = self.is_truthy_value(extensible)?;
        self.continue_proxy_get_own_after_target(
            site,
            mode,
            state,
            pending.values[PROXY_GET_OWN_DESCRIPTOR],
            target_descriptor,
            extensible,
        )
    }

    fn dispatch_proxy_get_own_target_descriptor(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_get_own_parent(site, mode, state, ProxyGetOwnStage::TargetGetOwn, target)?;
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
        self.resume_proxy_get_own(continuation, mode, ProxyGetOwnStage::TargetGetOwn, value)
    }

    fn dispatch_proxy_get_own_target_extensible(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_get_own_parent(
            site,
            mode,
            state,
            ProxyGetOwnStage::TargetIsExtensible,
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
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_get_own(
            continuation,
            mode,
            ProxyGetOwnStage::TargetIsExtensible,
            value,
        )
    }

    fn push_proxy_get_own_parent(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        stage: ProxyGetOwnStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_get_own(
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

    fn finish_proxy_get_own_mode(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        descriptor: Option<PropertyDescriptor>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let result = match mode {
            ProxyGetOwnMode::Descriptor => {
                let Some(descriptor) = descriptor else {
                    self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    )?;
                    return Ok(None);
                };
                let object = self.create_ordinary_object()?;
                self.materialize_property_descriptor(object, descriptor)?;
                object
            }
            ProxyGetOwnMode::HasOwn => boolean_value(descriptor.is_some()),
            ProxyGetOwnMode::Enumerable => boolean_value(
                descriptor
                    .and_then(PropertyDescriptor::enumerable)
                    .unwrap_or(false),
            ),
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }

    fn allocate_proxy_get_own_state(
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

    /// Completes a parsed Proxy descriptor and validates it against an ordinary target descriptor.
    pub(crate) fn finish_proxy_get_own_descriptor_parse(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        state: GcRef<NativeCallState>,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let target_descriptor = if pending.values[PROXY_GET_OWN_TARGET_DESCRIPTOR].as_immediate()
            == Some(Immediate::Undefined)
        {
            None
        } else {
            Some(self.parse_property_descriptor(pending.values[PROXY_GET_OWN_TARGET_DESCRIPTOR])?)
        };
        let extensible = self.is_truthy_value(pending.values[PROXY_ACTIVE_OBJECT])?;
        self.validate_proxy_descriptor_compatibility(descriptor, target_descriptor, extensible)?;
        self.finish_proxy_get_own_mode(site, mode, Some(descriptor))
            .map(|_| ())
    }

    /// Enforces the non-configurable/non-writable subset of IsCompatiblePropertyDescriptor.
    pub(super) fn validate_proxy_descriptor_compatibility(
        &mut self,
        descriptor: PropertyDescriptor,
        target: Option<PropertyDescriptor>,
        extensible: bool,
    ) -> Result<(), ExecutionError> {
        let Some(target) = target else {
            if !extensible || descriptor.configurable() == Some(false) {
                return Err(ExecutionError::ProxyInvariantViolation);
            }
            return Ok(());
        };
        if descriptor.configurable() == Some(false) && target.configurable() != Some(false) {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        if descriptor.configurable() == Some(false)
            && let (PropertyDescriptor::Data(proposed), PropertyDescriptor::Data(current)) =
                (descriptor, target)
            && proposed.writable == Some(false)
            && current.writable == Some(true)
        {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        if target.configurable() == Some(false) {
            if descriptor.configurable() == Some(true)
                || descriptor
                    .enumerable()
                    .is_some_and(|value| Some(value) != target.enumerable())
            {
                return Err(ExecutionError::ProxyInvariantViolation);
            }
            match (descriptor, target) {
                (PropertyDescriptor::Data(proposed), PropertyDescriptor::Data(current)) => {
                    if current.writable == Some(false)
                        && (proposed.writable == Some(true)
                            || proposed
                                .value
                                .is_some_and(|value| current.value != Some(value)))
                    {
                        return Err(ExecutionError::ProxyInvariantViolation);
                    }
                }
                (PropertyDescriptor::Accessor(proposed), PropertyDescriptor::Accessor(current)) => {
                    if proposed
                        .getter
                        .is_some_and(|value| current.getter != Some(value))
                        || proposed
                            .setter
                            .is_some_and(|value| current.setter != Some(value))
                    {
                        return Err(ExecutionError::ProxyInvariantViolation);
                    }
                }
                (PropertyDescriptor::Generic(_), _) => {}
                _ => return Err(ExecutionError::ProxyInvariantViolation),
            }
        }
        Ok(())
    }

    /// Publishes one retained Proxy state value with the matching old-to-young barrier.
    pub(super) fn update_proxy_state_value(
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
                .map(|_| ())
                .map_err(ExecutionError::HeapReference)
        })
    }
}
