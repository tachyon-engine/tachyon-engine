//! Proxy `[[DefineOwnProperty]]` dispatch and descriptor-preserving continuation state.

use super::*;

const PROXY_DEFINE_PENDING: usize = 3;
pub(crate) const PROXY_DEFINE_HANDLER: usize = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingProxyDefine {
    target: Value,
    key: PropertyKey,
    descriptor: PropertyDescriptor,
    target_descriptor: Option<PropertyDescriptor>,
    result_object: Value,
    active_proxy: Value,
    retained: Value,
    descriptor_object: Value,
}

impl Trace for PendingProxyDefine {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        if let Some(symbol) = self.key.symbol() {
            let mut value = symbol.value();
            value.trace(tracer);
        }
        trace_property_descriptor(&mut self.descriptor, tracer);
        if let Some(descriptor) = &mut self.target_descriptor {
            trace_property_descriptor(descriptor, tracer);
        }
        self.result_object.trace(tracer);
        self.active_proxy.trace(tracer);
        self.retained.trace(tracer);
        self.descriptor_object.trace(tracer);
    }
}

struct PendingProxyDefineRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingProxyDefine,
}

impl Trace for PendingProxyDefineRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

#[derive(Clone, Copy)]
enum DescriptorObjectField {
    Value,
    Writable,
    Get,
    Set,
    Enumerable,
    Configurable,
}

impl DescriptorObjectField {
    const ORDER: [Self; 6] = [
        Self::Value,
        Self::Writable,
        Self::Get,
        Self::Set,
        Self::Enumerable,
        Self::Configurable,
    ];

    const fn name(self) -> &'static [u8] {
        match self {
            Self::Value => b"value",
            Self::Writable => b"writable",
            Self::Get => b"get",
            Self::Set => b"set",
            Self::Enumerable => b"enumerable",
            Self::Configurable => b"configurable",
        }
    }
}

impl Isolate {
    /// Starts Proxy define after ToPropertyDescriptor has produced one presence-aware record.
    pub(crate) fn dispatch_proxy_define(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
        mode: ProxyDefineMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.dispatch_proxy_define_with_result(site, proxy, key, descriptor, proxy, mode)
    }

    /// Walks missing traps without materializing the descriptor object or allocating pending state.
    fn dispatch_proxy_define_with_result(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
        result_object: Value,
        mode: ProxyDefineMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"defineProperty")?;
            if self.is_proxy_value(snapshot.handler) {
                let state = self.allocate_pending_proxy_define(
                    snapshot.target,
                    key,
                    descriptor,
                    result_object,
                    proxy,
                    Value::from_immediate(Immediate::Undefined),
                )?;
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                )?;
                let pending_value = self.read(site.caller_base, site.destination)?;
                let state = self.pending_proxy_define_reference(pending_value)?;
                let active_proxy = self.pending_proxy_define_snapshot(state)?.active_proxy;
                let handler = self.proxy_snapshot(active_proxy)?.handler;
                return self.dispatch_proxy_define_handler_get(
                    site,
                    mode,
                    state,
                    handler,
                    trap_name.into(),
                );
            }
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_define(
                        site,
                        snapshot.target,
                        key,
                        descriptor,
                        result_object,
                        mode,
                    );
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
                        return self.forward_proxy_define(
                            site,
                            snapshot.target,
                            key,
                            descriptor,
                            result_object,
                            mode,
                        );
                    }
                    let state = self.allocate_pending_proxy_define(
                        snapshot.target,
                        key,
                        descriptor,
                        result_object,
                        proxy,
                        trap,
                    )?;
                    self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                    )?;
                    return self.prepare_proxy_define_trap_call(site, mode);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.forward_proxy_define(
                        site,
                        snapshot.target,
                        key,
                        descriptor,
                        result_object,
                        mode,
                    );
                }
                PropertyRead::Accessor(getter) => {
                    let state = self.allocate_pending_proxy_define(
                        snapshot.target,
                        key,
                        descriptor,
                        result_object,
                        proxy,
                        getter,
                    )?;
                    self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                    )?;
                    let pending = self.pending_proxy_define_snapshot(state)?;
                    let handler = self.proxy_snapshot(pending.active_proxy)?.handler;
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_define(
                            site,
                            mode,
                            ProxyDefineStage::TrapGetter,
                            Value::from_heap_ref(state.raw()),
                            handler,
                        ),
                        pending.retained,
                    );
                }
            }
        }
    }

    /// Reads `handler.defineProperty` through nested Proxy layers before applying GetMethod.
    fn dispatch_proxy_define_handler_get(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<PendingProxyDefine>,
        handler: Value,
        key: PropertyKey,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_define(
                site,
                mode,
                ProxyDefineStage::TrapGetter,
                Value::from_heap_ref(state.raw()),
                handler,
            ))
            .map_err(Self::completion_stack_error)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, handler, handler, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return outcome;
        }
        let continuation = self.pop_native_continuation()?;
        let trap = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_define(continuation, mode, ProxyDefineStage::TrapGetter, trap)
    }

    /// Resumes trap lookup/call and the two observable target invariant operations.
    pub(crate) fn resume_proxy_define(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxyDefineMode,
        stage: ProxyDefineStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match stage {
            ProxyDefineStage::TrapGetter => {
                let state = self.pending_proxy_define_reference(continuation.first())?;
                self.update_pending_proxy_define_retained(state, value)?;
                if matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                ) {
                    let pending = self.pending_proxy_define_snapshot(state)?;
                    return self.forward_pending_proxy_define(continuation.site(), mode, pending);
                }
                self.resolve_function_object(value)?;
                self.prepare_proxy_define_trap_call(continuation.site(), mode)
            }
            ProxyDefineStage::TrapCall => {
                let state = self.native_call_state_reference(continuation.first())?;
                self.finish_proxy_define_trap(continuation.site(), mode, state, value)
            }
            ProxyDefineStage::TargetGetOwn => {
                let state = self.native_call_state_reference(continuation.first())?;
                self.finish_proxy_define_target_descriptor(continuation.site(), mode, state, value)
            }
            ProxyDefineStage::TargetIsExtensible => {
                let state = self.native_call_state_reference(continuation.first())?;
                self.finish_proxy_define_invariant(continuation.site(), mode, state, value)
            }
        }
    }

    /// Creates FromPropertyDescriptor exactly once, then publishes the three-argument call state.
    fn prepare_proxy_define_trap_call(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending_value = self.read(site.caller_base, site.destination)?;
        let pending_state = self.pending_proxy_define_reference(pending_value)?;
        let trap = self.pending_proxy_define_snapshot(pending_state)?.retained;
        self.resolve_function_object(trap)?;
        let descriptor_object = self.create_ordinary_object()?;
        let pending_value = self.read(site.caller_base, site.destination)?;
        let pending_state = self.pending_proxy_define_reference(pending_value)?;
        self.update_pending_proxy_define_object(pending_state, descriptor_object)?;
        for field in DescriptorObjectField::ORDER {
            self.materialize_pending_proxy_define_field(site, field)?;
        }
        let pending_value = self.read(site.caller_base, site.destination)?;
        let pending_state = self.pending_proxy_define_reference(pending_value)?;
        let pending = self.pending_proxy_define_snapshot(pending_state)?;
        let key = match pending.key {
            PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
            PropertyKey::Symbol(symbol) => symbol.value(),
            PropertyKey::Private(_) => {
                return Err(ExecutionError::PrivatePropertyKeyEscaped);
            }
        };
        let pending_value = self.read(site.caller_base, site.destination)?;
        let pending_state = self.pending_proxy_define_reference(pending_value)?;
        let pending = self.pending_proxy_define_snapshot(pending_state)?;
        let handler = self.proxy_snapshot(pending.active_proxy)?.handler;
        let call_state = self.allocate_proxy_define_call_state(
            pending.target,
            key,
            pending.descriptor_object,
            pending_value,
            handler,
        )?;
        let pending_value = self.read(site.caller_base, site.destination)?;
        let pending_state = self.pending_proxy_define_reference(pending_value)?;
        let trap = self.pending_proxy_define_snapshot(pending_state)?.retained;
        self.dispatch_property_callback(
            NativeContinuation::proxy_define(
                site,
                mode,
                ProxyDefineStage::TrapCall,
                Value::from_heap_ref(call_state.raw()),
                trap,
            ),
            trap,
        )
    }

    /// Converts a trap result and starts target descriptor lookup only when it is true.
    fn finish_proxy_define_trap(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<NativeCallState>,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        if !self.is_truthy_value(trap_result)? {
            return self.finish_proxy_define_mode(site, mode, state, false);
        }
        let pending = self.native_call_state_snapshot(state)?;
        let target = pending.values[PROXY_TARGET_ARGUMENT];
        if self.is_proxy_value(target) {
            return self.dispatch_proxy_define_target_get_own(site, mode, state, target);
        }
        let key = self.property_key(pending.values[PROXY_HAS_KEY_ARGUMENT])?;
        let descriptor = self.complete_own_property_descriptor(target, key)?;
        let pending_state = self.proxy_define_pending_from_call_state(state)?;
        self.update_pending_proxy_define_target_descriptor(pending_state, descriptor)?;
        self.dispatch_proxy_define_target_extensible(site, mode, state, target)
    }

    /// Stores a nested Proxy target descriptor before continuing to extensibility.
    fn finish_proxy_define_target_descriptor(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<NativeCallState>,
        descriptor: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.update_proxy_state_value(state, PROXY_DEFINE_HANDLER, descriptor)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let descriptor = self.native_call_state_snapshot(state)?.values[PROXY_DEFINE_HANDLER];
        let descriptor = if descriptor.as_immediate() == Some(Immediate::Undefined) {
            None
        } else {
            Some(self.parse_property_descriptor(descriptor)?)
        };
        let pending_state = self.proxy_define_pending_from_call_state(state)?;
        self.update_pending_proxy_define_target_descriptor(pending_state, descriptor)?;
        let target = self.native_call_state_snapshot(state)?.values[PROXY_TARGET_ARGUMENT];
        self.dispatch_proxy_define_target_extensible(site, mode, state, target)
    }

    /// Applies compatibility, setting-config-false, and writable-strengthening invariants.
    fn finish_proxy_define_invariant(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<NativeCallState>,
        extensible: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let extensible = self.is_truthy_value(extensible)?;
        let pending_state = self.proxy_define_pending_from_call_state(state)?;
        let pending = self.pending_proxy_define_snapshot(pending_state)?;
        self.validate_proxy_descriptor_compatibility(
            pending.descriptor,
            pending.target_descriptor,
            extensible,
        )?;
        if let (PropertyDescriptor::Data(proposed), Some(PropertyDescriptor::Data(current))) =
            (pending.descriptor, pending.target_descriptor)
            && current.configurable == Some(false)
            && current.writable == Some(true)
            && proposed.writable == Some(false)
        {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        self.finish_proxy_define_mode(site, mode, state, true)
    }

    /// Materializes one present normalized descriptor field while reacquiring moved state each time.
    fn materialize_pending_proxy_define_field(
        &mut self,
        site: NativeContinuationSite,
        field: DescriptorObjectField,
    ) -> Result<(), ExecutionError> {
        let atom = self.intern_intrinsic_name(field.name())?;
        let pending_value = self.read(site.caller_base, site.destination)?;
        let state = self.pending_proxy_define_reference(pending_value)?;
        let pending = self.pending_proxy_define_snapshot(state)?;
        let value = descriptor_object_field_value(pending.descriptor, field);
        if let Some(value) = value {
            self.set_own_data_property(pending.descriptor_object, atom, value)?;
        }
        Ok(())
    }

    /// Suspends the outer invariant while a Proxy target resolves its own descriptor.
    fn dispatch_proxy_define_target_get_own(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_define_parent(site, mode, state, ProxyDefineStage::TargetGetOwn, target)?;
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
        self.resume_proxy_define(
            continuation,
            mode,
            ProxyDefineStage::TargetGetOwn,
            descriptor,
        )
    }

    /// Suspends the outer invariant while the target reports extensibility.
    fn dispatch_proxy_define_target_extensible(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<NativeCallState>,
        target: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if !self.is_proxy_value(target) {
            let extensible = self.object_snapshot(target)?.1.extensible;
            return self.finish_proxy_define_invariant(
                site,
                mode,
                state,
                boolean_value(extensible),
            );
        }
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_define_parent(
            site,
            mode,
            state,
            ProxyDefineStage::TargetIsExtensible,
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
        self.resume_proxy_define(
            continuation,
            mode,
            ProxyDefineStage::TargetIsExtensible,
            extensible,
        )
    }

    /// Pushes one traced parent around a nested target internal method.
    fn push_proxy_define_parent(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<NativeCallState>,
        stage: ProxyDefineStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_define(
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

    /// Forwards a missing trap to an ordinary target while preserving the outer result object.
    fn forward_proxy_define(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
        result_object: Value,
        mode: ProxyDefineMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let success = match self.define_property(target, key, descriptor) {
            Ok(()) => true,
            Err(
                ExecutionError::NonExtensibleObject(_)
                | ExecutionError::InvalidPropertyRedefinition(_),
            ) => false,
            Err(error) => return Err(error),
        };
        self.finish_proxy_define_result(site, mode, result_object, success)
    }

    fn forward_pending_proxy_define(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        pending: PendingProxyDefine,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if self.is_proxy_value(pending.target) {
            return self.dispatch_proxy_define_with_result(
                site,
                pending.target,
                pending.key,
                pending.descriptor,
                pending.result_object,
                mode,
            );
        }
        self.forward_proxy_define(
            site,
            pending.target,
            pending.key,
            pending.descriptor,
            pending.result_object,
            mode,
        )
    }

    fn finish_proxy_define_mode(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        state: GcRef<NativeCallState>,
        success: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending_state = self.proxy_define_pending_from_call_state(state)?;
        let result_object = self
            .pending_proxy_define_snapshot(pending_state)?
            .result_object;
        self.finish_proxy_define_result(site, mode, result_object, success)
    }

    /// Maps the internal boolean to Object.defineProperty or Reflect.defineProperty.
    fn finish_proxy_define_result(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        result_object: Value,
        success: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if !success
            && matches!(
                mode,
                ProxyDefineMode::Object | ProxyDefineMode::LegacyAccessor
            )
        {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        let result = if mode == ProxyDefineMode::Reflect {
            boolean_value(success)
        } else if mode == ProxyDefineMode::LegacyAccessor {
            Value::from_immediate(Immediate::Undefined)
        } else {
            result_object
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }

    fn pending_proxy_define_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingProxyDefine>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_proxy_define)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn pending_proxy_define_snapshot(
        &mut self,
        state: GcRef<PendingProxyDefine>,
    ) -> Result<PendingProxyDefine, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_proxy_define)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn pending_proxy_define_handler(
        &mut self,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        let state = self.pending_proxy_define_reference(value)?;
        let active_proxy = self.pending_proxy_define_snapshot(state)?.active_proxy;
        self.proxy_snapshot(active_proxy).map(|proxy| proxy.handler)
    }

    fn proxy_define_pending_from_call_state(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<GcRef<PendingProxyDefine>, ExecutionError> {
        let value = self.native_call_state_snapshot(state)?.values[PROXY_DEFINE_PENDING];
        self.pending_proxy_define_reference(value)
    }

    fn update_pending_proxy_define_retained(
        &mut self,
        state: GcRef<PendingProxyDefine>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.update_pending_proxy_define_value(state, value, |pending, value| {
            pending.retained = value;
        })
    }

    fn update_pending_proxy_define_object(
        &mut self,
        state: GcRef<PendingProxyDefine>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.update_pending_proxy_define_value(state, value, |pending, value| {
            pending.descriptor_object = value;
        })
    }

    fn update_pending_proxy_define_value(
        &mut self,
        state: GcRef<PendingProxyDefine>,
        value: Value,
        update: impl FnOnce(&mut PendingProxyDefine, Value),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_proxy_define)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending, value);
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map(|_| ())
                .map_err(ExecutionError::HeapReference)
        })
    }

    /// Publishes the captured target descriptor and all of its possible heap edges.
    fn update_pending_proxy_define_target_descriptor(
        &mut self,
        state: GcRef<PendingProxyDefine>,
        descriptor: Option<PropertyDescriptor>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_proxy_define)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .target_descriptor = descriptor;
                Ok::<(), ExecutionError>(())
            })?;
            for value in descriptor.into_iter().flat_map(property_descriptor_edges) {
                scope
                    .write_value_barrier(state, value)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Allocates the descriptor-preserving state before FromPropertyDescriptor can collect.
    fn allocate_pending_proxy_define(
        &mut self,
        target: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
        result_object: Value,
        active_proxy: Value,
        retained: Value,
    ) -> Result<GcRef<PendingProxyDefine>, ExecutionError> {
        let mut roots = PendingProxyDefineRoots {
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
            pending: PendingProxyDefine {
                target,
                key,
                descriptor,
                target_descriptor: None,
                result_object,
                active_proxy,
                retained,
                descriptor_object: Value::from_immediate(Immediate::Undefined),
            },
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_proxy_define,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates the final `(target,key,descObj)` source plus pending state and captured handler.
    fn allocate_proxy_define_call_state(
        &mut self,
        target: Value,
        key: Value,
        descriptor_object: Value,
        pending: Value,
        handler: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let values = [target, key, descriptor_object, pending, handler];
        let mut roots = NativeCallStateRoots {
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

fn trace_property_descriptor(descriptor: &mut PropertyDescriptor, tracer: &mut dyn Tracer) {
    match descriptor {
        PropertyDescriptor::Data(descriptor) => descriptor.value.trace(tracer),
        PropertyDescriptor::Accessor(descriptor) => {
            descriptor.getter.trace(tracer);
            descriptor.setter.trace(tracer);
        }
        PropertyDescriptor::Generic(_) => {}
    }
}

fn property_descriptor_edges(descriptor: PropertyDescriptor) -> impl Iterator<Item = Value> {
    let values = match descriptor {
        PropertyDescriptor::Data(descriptor) => [descriptor.value, None],
        PropertyDescriptor::Accessor(descriptor) => [descriptor.getter, descriptor.setter],
        PropertyDescriptor::Generic(_) => [None, None],
    };
    values.into_iter().flatten()
}

fn descriptor_object_field_value(
    descriptor: PropertyDescriptor,
    field: DescriptorObjectField,
) -> Option<Value> {
    let boolean = |value| boolean_value(value);
    match (descriptor, field) {
        (PropertyDescriptor::Data(descriptor), DescriptorObjectField::Value) => descriptor.value,
        (PropertyDescriptor::Data(descriptor), DescriptorObjectField::Writable) => {
            descriptor.writable.map(boolean)
        }
        (PropertyDescriptor::Accessor(descriptor), DescriptorObjectField::Get) => descriptor.getter,
        (PropertyDescriptor::Accessor(descriptor), DescriptorObjectField::Set) => descriptor.setter,
        (descriptor, DescriptorObjectField::Enumerable) => descriptor.enumerable().map(boolean),
        (descriptor, DescriptorObjectField::Configurable) => descriptor.configurable().map(boolean),
        _ => None,
    }
}
