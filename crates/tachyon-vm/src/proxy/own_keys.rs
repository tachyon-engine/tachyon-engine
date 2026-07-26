//! Resumable Proxy `[[OwnPropertyKeys]]` dispatch and target invariants.

use core::mem::size_of;

use super::*;

const OWN_KEYS_TARGET: usize = 0;
const OWN_KEYS_ACTIVE_PROXY: usize = 1;
const OWN_KEYS_TRAP_RESULT: usize = 2;
const OWN_KEYS_HANDLER: usize = 3;

/// GC-owned state for one Proxy ownKeys operation.
///
/// The two boxed key lists are allocated exactly after their lengths are known.  Keeping the
/// lists in managed state makes accessor/trap calls and forced collection independent of the Rust
/// stack; no callback is allowed to borrow either list across a safepoint.
#[derive(Debug)]
pub(crate) struct PendingProxyOwnKeys {
    target: Value,
    active_proxy: Value,
    handler: Value,
    trap_result: Value,
    mode: ProxyOwnKeysMode,
    length: u32,
    index: u32,
    complete: bool,
    keys: Box<[PropertyKey]>,
    key_membership: Box<[u64]>,
    target_keys: Box<[PropertyKey]>,
}

impl Trace for PendingProxyOwnKeys {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.active_proxy.trace(tracer);
        self.handler.trace(tracer);
        self.trap_result.trace(tracer);
        self.keys.trace(tracer);
        self.target_keys.trace(tracer);
    }
}

impl GcExternalMemory for PendingProxyOwnKeys {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.keys
            .len()
            .saturating_add(self.target_keys.len())
            .saturating_mul(size_of::<PropertyKey>())
            .saturating_add(self.key_membership.len().saturating_mul(size_of::<u64>()))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct OwnKeysSnapshot {
    pub(crate) target: Value,
    pub(crate) active_proxy: Value,
    pub(crate) handler: Value,
    pub(crate) trap_result: Value,
    pub(crate) length: u32,
    pub(crate) index: u32,
    pub(crate) complete: bool,
    pub(crate) mode: ProxyOwnKeysMode,
}

impl Isolate {
    /// Starts one Proxy ownKeys operation, iterating through synchronous nullish trap chains.
    pub(crate) fn dispatch_proxy_own_keys(
        &mut self,
        site: NativeContinuationSite,
        mut proxy: Value,
        mode: ProxyOwnKeysMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let snapshot = self.proxy_snapshot(proxy)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"ownKeys")?;
            if self.is_proxy_value(snapshot.handler) {
                let state = self.allocate_proxy_own_keys_state(
                    proxy,
                    snapshot.target,
                    snapshot.handler,
                    Value::from_immediate(Immediate::Undefined),
                    mode,
                )?;
                return self.dispatch_proxy_own_keys_handler_get(
                    site,
                    state,
                    snapshot.handler,
                    trap_name.into(),
                );
            }
            match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.finish_proxy_own_keys_forward(site, proxy, snapshot.target, mode);
                }
                PropertyRead::Data(trap)
                    if matches!(
                        trap.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.finish_proxy_own_keys_forward(site, proxy, snapshot.target, mode);
                }
                PropertyRead::Data(trap) => {
                    let state = self.allocate_proxy_own_keys_state(
                        proxy,
                        snapshot.target,
                        snapshot.handler,
                        trap,
                        mode,
                    )?;
                    return self.continue_proxy_own_keys_lookup(site, state, trap);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    if self.is_proxy_value(snapshot.target) {
                        proxy = snapshot.target;
                        continue;
                    }
                    return self.finish_proxy_own_keys_forward(site, proxy, snapshot.target, mode);
                }
                PropertyRead::Accessor(getter) => {
                    let state = self.allocate_proxy_own_keys_state(
                        proxy,
                        snapshot.target,
                        snapshot.handler,
                        getter,
                        mode,
                    )?;
                    return self.dispatch_property_callback(
                        NativeContinuation::proxy_own_keys(
                            site,
                            ProxyOwnKeysMode::Internal,
                            ProxyOwnKeysStage::TrapGetter,
                            Value::from_heap_ref(state.raw()),
                            snapshot.handler,
                        ),
                        getter,
                    );
                }
            }
        }
    }

    /// Resumes trap lookup, array-like reads, and nested target ownKeys calls.
    pub(crate) fn resume_proxy_own_keys(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxyOwnKeysMode,
        stage: ProxyOwnKeysStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if stage == ProxyOwnKeysStage::IntegrityExtensible {
            return self.resume_proxy_test_integrity_extensible(continuation, mode, value);
        }
        let state = self.pending_proxy_own_keys_reference(continuation.first())?;
        match stage {
            ProxyOwnKeysStage::TrapGetter => {
                self.continue_proxy_own_keys_lookup(continuation.site(), state, value)
            }
            ProxyOwnKeysStage::TrapCall => {
                self.finish_proxy_own_keys_trap(continuation.site(), state, value)
            }
            ProxyOwnKeysStage::LengthGet => {
                self.continue_proxy_own_keys_length(continuation.site(), state, value)
            }
            ProxyOwnKeysStage::ElementGet => {
                self.continue_proxy_own_keys_element(continuation.site(), state, value)
            }
            ProxyOwnKeysStage::TargetOwnKeys => {
                self.finish_proxy_own_keys_nested_target(continuation.site(), state, value)
            }
            ProxyOwnKeysStage::IntegrityDescriptor => {
                self.resume_proxy_integrity_descriptor(continuation, state, value)
            }
            ProxyOwnKeysStage::IntegrityExtensible => unreachable!("handled before state lookup"),
        }
    }

    /// Starts Proxy TestIntegrityLevel with the required [[IsExtensible]] observation.
    pub(crate) fn begin_proxy_test_integrity(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        freeze: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let mode = if freeze {
            ProxyOwnKeysMode::IntegrityFrozen
        } else {
            ProxyOwnKeysMode::IntegritySealed
        };
        let continuation = NativeContinuation::proxy_own_keys(
            site,
            mode,
            ProxyOwnKeysStage::IntegrityExtensible,
            proxy,
            Value::from_immediate(Immediate::Undefined),
        );
        self.dispatch_proxy_integrity_operation(continuation, |isolate| {
            isolate.dispatch_proxy_internal_method(site, proxy, ProxyInternalMethod::IsExtensible)
        })
    }

    fn resume_proxy_test_integrity_extensible(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxyOwnKeysMode,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        if self.is_truthy_value(value)? {
            self.write(site.caller_base, site.destination, boolean_value(false))?;
            return Ok(None);
        }
        self.dispatch_proxy_own_keys(site, continuation.first(), mode)
    }

    /// Returns a checked state reference for the eventual ownKeys consumer.
    pub(crate) fn pending_proxy_own_keys_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingProxyOwnKeys>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_proxy_own_keys)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies the completed key list for a consumer without exposing a managed borrow.
    pub(crate) fn pending_proxy_own_keys_values(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<Vec<PropertyKey>, ExecutionError> {
        let (_, len) = self.proxy_own_keys_state_meta(state)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(len)
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                output.extend_from_slice(&state.keys);
                Ok::<(), ExecutionError>(())
            })
        })?;
        Ok(output)
    }

    /// Reads only callback receiver fields while the continuation keeps the state rooted.
    pub(crate) fn proxy_own_keys_snapshot_for_callback(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<OwnKeysSnapshot, ExecutionError> {
        self.proxy_own_keys_snapshot(state)
    }

    /// Copies target keys without retaining a managed borrow across invariant checks.
    fn pending_proxy_own_keys_target_values(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<Vec<PropertyKey>, ExecutionError> {
        let mut output = Vec::new();
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                output
                    .try_reserve_exact(state.target_keys.len())
                    .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
                output.extend_from_slice(&state.target_keys);
                Ok::<(), ExecutionError>(())
            })
        })?;
        Ok(output)
    }

    fn continue_proxy_own_keys_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        mut trap: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        loop {
            let pending = self.proxy_own_keys_snapshot(state)?;
            if matches!(
                trap.as_immediate(),
                Some(Immediate::Undefined | Immediate::Null)
            ) {
                if self.is_proxy_value(pending.target) {
                    let next_proxy = pending.target;
                    self.update_proxy_own_keys_value(state, OWN_KEYS_ACTIVE_PROXY, next_proxy)?;
                    self.update_proxy_own_keys_value(state, OWN_KEYS_TARGET, next_proxy)?;
                    let snapshot = self.proxy_snapshot(next_proxy)?;
                    if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                        return Err(ExecutionError::ProxyRevoked);
                    }
                    let trap_name = self.intern_intrinsic_name(b"ownKeys")?;
                    self.update_proxy_own_keys_value(state, OWN_KEYS_HANDLER, snapshot.handler)?;
                    if self.is_proxy_value(snapshot.handler) {
                        return self.dispatch_proxy_own_keys_handler_get(
                            site,
                            state,
                            snapshot.handler,
                            trap_name.into(),
                        );
                    }
                    trap = match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                        PropertyRead::Missing => Value::from_immediate(Immediate::Undefined),
                        PropertyRead::Data(value) => value,
                        PropertyRead::Accessor(getter)
                            if getter.as_immediate() == Some(Immediate::Undefined) =>
                        {
                            Value::from_immediate(Immediate::Undefined)
                        }
                        PropertyRead::Accessor(getter) => {
                            return self.dispatch_property_callback(
                                NativeContinuation::proxy_own_keys(
                                    site,
                                    ProxyOwnKeysMode::Internal,
                                    ProxyOwnKeysStage::TrapGetter,
                                    Value::from_heap_ref(state.raw()),
                                    snapshot.handler,
                                ),
                                getter,
                            );
                        }
                    };
                    continue;
                }
                return self.finish_proxy_own_keys_forward(
                    site,
                    pending.active_proxy,
                    pending.target,
                    pending.mode,
                );
            }
            self.resolve_function_object(trap)?;
            return self.dispatch_property_callback(
                NativeContinuation::proxy_own_keys(
                    site,
                    ProxyOwnKeysMode::Internal,
                    ProxyOwnKeysStage::TrapCall,
                    Value::from_heap_ref(state.raw()),
                    trap,
                ),
                trap,
            );
        }
    }

    /// Reads `handler.ownKeys` through nested Proxy layers while retaining operation state.
    fn dispatch_proxy_own_keys_handler_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        handler: Value,
        key: PropertyKey,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_own_keys(
                site,
                ProxyOwnKeysMode::Internal,
                ProxyOwnKeysStage::TrapGetter,
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
            || self.fiber.completions.len() == completion_depth
        {
            return outcome;
        }
        let continuation = self.pop_native_continuation()?;
        let trap = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_own_keys(
            continuation,
            ProxyOwnKeysMode::Internal,
            ProxyOwnKeysStage::TrapGetter,
            trap,
        )
    }

    fn finish_proxy_own_keys_trap(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if !self.is_object_value(trap_result) {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        self.update_proxy_own_keys_value(state, OWN_KEYS_TRAP_RESULT, trap_result)?;
        self.reset_proxy_own_keys_list(state)?;
        self.advance_proxy_own_keys_length(site, state, None)
    }

    fn advance_proxy_own_keys_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        returned: Option<Value>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.proxy_own_keys_snapshot(state)?;
        if let Some(value) = returned {
            let length = self.proxy_own_keys_to_length(value)?;
            self.allocate_proxy_own_keys_list(state, length)?;
            return self.advance_proxy_own_keys_elements(site, state, None);
        }
        let key = PropertyKey::Atom(self.length_atom()?);
        self.begin_proxy_own_keys_get(
            site,
            state,
            pending.trap_result,
            key,
            ProxyOwnKeysStage::LengthGet,
        )
    }

    fn continue_proxy_own_keys_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.advance_proxy_own_keys_length(site, state, Some(value))
    }

    /// Drains synchronous trap-result elements iteratively and yields only for observable reads.
    fn advance_proxy_own_keys_elements(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        mut returned: Option<Value>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            if let Some(value) = returned.take() {
                if !self.is_string_value(value) && !self.is_symbol_value(value) {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                let key = self.property_key(value)?;
                let pending = self.proxy_own_keys_snapshot(state)?;
                if self.proxy_own_keys_contains(state, key)? {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                self.store_proxy_own_keys_key(state, pending.index, key)?;
            }
            let pending = self.proxy_own_keys_snapshot(state)?;
            if pending.index == pending.length {
                return self.begin_proxy_own_keys_target(site, state);
            }
            let key = PropertyKey::Atom(self.safe_integer_property_atom(u64::from(pending.index))?);
            match self.resolve_property_read_until_proxy(pending.trap_result, key)? {
                PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                    returned = Some(value);
                }
                PropertyReadResolution::Read(PropertyRead::Missing) => {
                    returned = Some(Value::from_immediate(Immediate::Undefined));
                }
                PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    returned = Some(Value::from_immediate(Immediate::Undefined));
                }
                PropertyReadResolution::Read(PropertyRead::Accessor(_))
                | PropertyReadResolution::Proxy(_) => {
                    return self.begin_proxy_own_keys_get(
                        site,
                        state,
                        pending.trap_result,
                        key,
                        ProxyOwnKeysStage::ElementGet,
                    );
                }
            }
        }
    }

    fn continue_proxy_own_keys_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.advance_proxy_own_keys_elements(site, state, Some(value))
    }

    fn begin_proxy_own_keys_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        source: Value,
        key: PropertyKey,
        stage: ProxyOwnKeysStage,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match self.resolve_property_read_until_proxy(source, key)? {
            PropertyReadResolution::Proxy(_)
            | PropertyReadResolution::Read(PropertyRead::Accessor(_)) => {
                let depth = self.fiber.completions.len();
                let frames = self.fiber.frames.len();
                self.push_proxy_own_keys_continuation(site, state, stage, source)?;
                let result = self.dispatch_proxy_aware_property_read(site, source, source, key);
                let outcome = match result {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        if self.fiber.completions.len() > depth {
                            self.pop_native_continuation()?;
                        }
                        return Err(error);
                    }
                };
                if self.fiber.completions.len() == depth || self.fiber.frames.len() != frames {
                    return Ok(outcome);
                }
                let continuation = self.pop_native_continuation()?;
                let value = self.read(site.caller_base, site.destination)?;
                self.resume_proxy_own_keys(continuation, ProxyOwnKeysMode::Internal, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Missing) => self.resume_proxy_own_keys(
                NativeContinuation::proxy_own_keys(
                    site,
                    ProxyOwnKeysMode::Internal,
                    stage,
                    Value::from_heap_ref(state.raw()),
                    Value::from_immediate(Immediate::Undefined),
                ),
                ProxyOwnKeysMode::Internal,
                stage,
                Value::from_immediate(Immediate::Undefined),
            ),
            PropertyReadResolution::Read(PropertyRead::Data(value)) => self.resume_proxy_own_keys(
                NativeContinuation::proxy_own_keys(
                    site,
                    ProxyOwnKeysMode::Internal,
                    stage,
                    Value::from_heap_ref(state.raw()),
                    Value::from_immediate(Immediate::Undefined),
                ),
                ProxyOwnKeysMode::Internal,
                stage,
                value,
            ),
        }
    }

    fn begin_proxy_own_keys_target(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.proxy_own_keys_snapshot(state)?;
        if self.is_proxy_value(pending.target) {
            let depth = self.fiber.completions.len();
            let frames = self.fiber.frames.len();
            self.push_proxy_own_keys_continuation(
                site,
                state,
                ProxyOwnKeysStage::TargetOwnKeys,
                pending.target,
            )?;
            let result =
                self.dispatch_proxy_own_keys(site, pending.target, ProxyOwnKeysMode::Internal);
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    if self.fiber.completions.len() > depth {
                        self.pop_native_continuation()?;
                    }
                    return Err(error);
                }
            };
            if self.fiber.completions.len() == depth || self.fiber.frames.len() != frames {
                return Ok(outcome);
            }
            self.pop_native_continuation()?;
            let value = self.read(site.caller_base, site.destination)?;
            return self.finish_proxy_own_keys_nested_target(site, state, value);
        }
        let (_, snapshot) = self.object_snapshot(pending.target)?;
        let mut keys = Vec::new();
        let source = self.ordinary_own_property_keys(pending.target, snapshot)?;
        keys.try_reserve_exact(source.len())
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        keys.extend(source);
        self.set_proxy_own_keys_target_keys(state, keys.into_boxed_slice())?;
        self.finish_proxy_own_keys_invariants(site, state)
    }

    fn finish_proxy_own_keys_nested_target(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let nested = self.pending_proxy_own_keys_reference(value)?;
        let keys = self.pending_proxy_own_keys_values(nested)?;
        self.set_proxy_own_keys_target_keys(state, keys.into_boxed_slice())?;
        self.finish_proxy_own_keys_invariants(site, state)
    }

    fn finish_proxy_own_keys_invariants(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.proxy_own_keys_snapshot(state)?;
        let keys = self.pending_proxy_own_keys_values(state)?;
        debug_assert!(pending.complete);
        let target_keys = self.pending_proxy_own_keys_target_values(state)?;
        let target_extensible = self.proxy_own_keys_target_extensible(pending.target)?;
        if !self.is_proxy_value(pending.target) {
            for key in target_keys.iter().copied() {
                let Some(descriptor) =
                    self.complete_own_property_descriptor(pending.target, key)?
                else {
                    continue;
                };
                if descriptor.configurable() == Some(false)
                    && !self.proxy_own_keys_contains(state, key)?
                {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
            }
        }
        if !target_extensible
            && (keys.len() != target_keys.len()
                || keys.iter().any(|key| !target_keys.contains(key)))
        {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        self.set_proxy_own_keys_complete(state)?;
        self.materialize_proxy_own_keys(site, state, pending.mode)
    }

    /// Reads extensibility through nullish-trap Proxy layers without entering Rust recursion.
    fn proxy_own_keys_target_extensible(
        &mut self,
        mut target: Value,
    ) -> Result<bool, ExecutionError> {
        loop {
            if !self.is_proxy_value(target) {
                return Ok(self.object_snapshot(target)?.1.extensible);
            }
            let snapshot = self.proxy_snapshot(target)?;
            let name = self.intern_intrinsic_name(b"isExtensible")?;
            match self.resolve_property_read(snapshot.handler, name.into())? {
                PropertyRead::Missing => target = snapshot.target,
                PropertyRead::Data(value)
                    if matches!(
                        value.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) =>
                {
                    target = snapshot.target;
                }
                PropertyRead::Data(_) | PropertyRead::Accessor(_) => {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
            }
        }
    }

    fn finish_proxy_own_keys_forward(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        target: Value,
        mode: ProxyOwnKeysMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let handler = self.proxy_snapshot(proxy)?.handler;
        let state = self.allocate_proxy_own_keys_state(
            proxy,
            target,
            handler,
            Value::from_immediate(Immediate::Undefined),
            mode,
        )?;
        let (_, snapshot) = self.object_snapshot(target)?;
        let source = self.ordinary_own_property_keys(target, snapshot)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(source.len())
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        keys.extend(source);
        self.set_proxy_own_keys_target_keys(state, keys.clone().into_boxed_slice())?;
        self.set_proxy_own_keys_keys(state, keys.into_boxed_slice())?;
        self.set_proxy_own_keys_complete(state)?;
        self.materialize_proxy_own_keys(site, state, mode)
    }

    /// Materializes the completed internal key list for one Object/Reflect consumer.
    fn materialize_proxy_own_keys(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        mode: ProxyOwnKeysMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if matches!(
            mode,
            ProxyOwnKeysMode::IntegritySealed | ProxyOwnKeysMode::IntegrityFrozen
        ) {
            self.reset_proxy_own_keys_index(state)?;
            return self.advance_proxy_integrity_descriptors(site, state);
        }
        let pending = self.proxy_own_keys_snapshot(state)?;
        let keys = self.pending_proxy_own_keys_values(state)?;
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..self.call_site_from_native(site)
        })?;
        let mut output_index = 0_u64;
        for key in keys {
            let include = match mode {
                ProxyOwnKeysMode::Internal | ProxyOwnKeysMode::Reflect => true,
                ProxyOwnKeysMode::Names | ProxyOwnKeysMode::EnumerableNames => key.atom().is_some(),
                ProxyOwnKeysMode::Symbols => key.symbol().is_some(),
                ProxyOwnKeysMode::IntegritySealed | ProxyOwnKeysMode::IntegrityFrozen => {
                    unreachable!("integrity modes do not materialize key arrays")
                }
            };
            if !include {
                continue;
            }
            if mode == ProxyOwnKeysMode::EnumerableNames {
                let Some(descriptor) =
                    self.complete_own_property_descriptor(pending.target, key)?
                else {
                    continue;
                };
                if descriptor.enumerable() != Some(true) {
                    continue;
                }
            }
            let value = match key {
                PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
                PropertyKey::Symbol(symbol) => symbol.value(),
                PropertyKey::Private(_) => return Err(ExecutionError::PrivatePropertyKeyEscaped),
            };
            let output_key = self.safe_integer_property_atom(output_index)?;
            self.set_own_data_property(result, output_key, value)?;
            output_index = output_index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
        }
        let length = self.length_atom()?;
        self.set_own_data_property(result, length, safe_integer_value(output_index))?;
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }

    /// Dispatches one ordered Proxy [[GetOwnProperty]] query or completes the integrity result.
    fn advance_proxy_integrity_descriptors(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.proxy_own_keys_snapshot(state)?;
        let Some(key) = self.proxy_own_keys_current_key(state)? else {
            self.write(site.caller_base, site.destination, boolean_value(true))?;
            return Ok(None);
        };
        let key_value = match key {
            PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
            PropertyKey::Symbol(symbol) => symbol.value(),
            PropertyKey::Private(_) => return Err(ExecutionError::PrivatePropertyKeyEscaped),
        };
        let continuation = NativeContinuation::proxy_own_keys(
            site,
            pending.mode,
            ProxyOwnKeysStage::IntegrityDescriptor,
            Value::from_heap_ref(state.raw()),
            key_value,
        );
        self.dispatch_proxy_integrity_operation(continuation, |isolate| {
            isolate.dispatch_proxy_get_own(
                site,
                pending.active_proxy,
                key_value,
                ProxyGetOwnMode::Descriptor,
            )
        })
    }

    fn resume_proxy_integrity_descriptor(
        &mut self,
        continuation: NativeContinuation,
        state: GcRef<PendingProxyOwnKeys>,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if value.as_immediate() != Some(Immediate::Undefined) {
            let descriptor = self.parse_property_descriptor(value)?;
            let freeze =
                self.proxy_own_keys_snapshot(state)?.mode == ProxyOwnKeysMode::IntegrityFrozen;
            if descriptor.configurable() == Some(true)
                || (freeze
                    && matches!(
                        descriptor,
                        PropertyDescriptor::Data(DataPropertyDescriptor {
                            writable: Some(true),
                            ..
                        })
                    ))
            {
                let site = continuation.site();
                self.write(site.caller_base, site.destination, boolean_value(false))?;
                return Ok(None);
            }
        }
        self.advance_proxy_own_keys_index(state)?;
        self.advance_proxy_integrity_descriptors(continuation.site(), state)
    }

    /// Runs a child Proxy operation and drains the parent when it completed synchronously.
    fn dispatch_proxy_integrity_operation(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let outcome = operation(self);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return outcome;
        }
        let continuation = self.pop_native_continuation()?;
        let site = continuation.site();
        let value = self.read(site.caller_base, site.destination)?;
        let NativeContinuationKind::ProxyOwnKeys { mode, stage } = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_proxy_own_keys(continuation, mode, stage, value)
    }

    fn call_site_from_native(&self, site: NativeContinuationSite) -> CallSite {
        CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: Value::from_immediate(Immediate::Undefined),
            argument_base: 0,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }
    }

    fn allocate_proxy_own_keys_state(
        &mut self,
        active_proxy: Value,
        target: Value,
        handler: Value,
        trap_result: Value,
        mode: ProxyOwnKeysMode,
    ) -> Result<GcRef<PendingProxyOwnKeys>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_proxy_own_keys,
                0,
                PendingProxyOwnKeys {
                    target,
                    active_proxy,
                    handler,
                    trap_result,
                    mode,
                    length: 0,
                    index: 0,
                    complete: false,
                    keys: Box::new([]),
                    key_membership: Box::new([]),
                    target_keys: Box::new([]),
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn proxy_own_keys_snapshot(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<OwnKeysSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(OwnKeysSnapshot {
                    target: state.target,
                    active_proxy: state.active_proxy,
                    handler: state.handler,
                    trap_result: state.trap_result,
                    length: state.length,
                    index: state.index,
                    complete: state.complete,
                    mode: state.mode,
                })
            })
        })
    }

    fn proxy_own_keys_state_meta(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<(bool, usize), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok((state.complete, state.keys.len()))
            })
        })
    }

    fn update_proxy_own_keys_value(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                match slot {
                    OWN_KEYS_TARGET => state.target = value,
                    OWN_KEYS_ACTIVE_PROXY => state.active_proxy = value,
                    OWN_KEYS_TRAP_RESULT => state.trap_result = value,
                    OWN_KEYS_HANDLER => state.handler = value,
                    _ => return Err(ExecutionError::MissingNativeContinuation),
                }
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map(|_| ())
                .map_err(ExecutionError::HeapReference)
        })
    }

    fn reset_proxy_own_keys_list(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.length = 0;
                state.index = 0;
                state.complete = false;
                state.keys = Box::new([]);
                state.key_membership = Box::new([]);
                state.target_keys = Box::new([]);
                Ok::<(), ExecutionError>(())
            })
        })
    }

    fn reset_proxy_own_keys_index(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)
                    .map(|state| state.index = 0)
            })
        })
    }

    fn advance_proxy_own_keys_index(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.index = state
                    .index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
                Ok(())
            })
        })
    }

    fn proxy_own_keys_current_key(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<Option<PropertyKey>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)
                    .map(|state| state.keys.get(state.index as usize).copied())
            })
        })
    }

    fn allocate_proxy_own_keys_list(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
        length: u32,
    ) -> Result<(), ExecutionError> {
        let length = usize::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(length)
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        keys.resize(length, PropertyKey::Atom(self.length_atom()?));
        let membership_length = length
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(ExecutionError::OwnPropertyKeyAllocationFailed)?;
        let mut membership = Vec::new();
        membership
            .try_reserve_exact(membership_length)
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        membership.resize(membership_length, 0);
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.length =
                    u32::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
                state.index = 0;
                state.keys = keys.into_boxed_slice();
                state.key_membership = membership.into_boxed_slice();
                Ok::<(), ExecutionError>(())
            })
        })
    }

    fn store_proxy_own_keys_key(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
        index: u32,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let slot = state
                    .keys
                    .get_mut(index as usize)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                *slot = key;
                proxy_own_keys_membership_insert(&mut state.key_membership, key)?;
                state.index = index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
                Ok::<(), ExecutionError>(())
            })?;
            if let PropertyKey::Symbol(symbol) = key {
                scope
                    .write_value_barrier(state, symbol.value())
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok::<(), ExecutionError>(())
        })
    }

    fn proxy_own_keys_contains(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
        key: PropertyKey,
    ) -> Result<bool, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(proxy_own_keys_membership_contains(
                    &state.key_membership,
                    key,
                ))
            })
        })
    }

    fn set_proxy_own_keys_keys(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
        keys: Box<[PropertyKey]>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .keys = keys;
                Ok::<(), ExecutionError>(())
            })
        })
    }

    fn set_proxy_own_keys_target_keys(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
        keys: Box<[PropertyKey]>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .target_keys = keys;
                Ok::<(), ExecutionError>(())
            })
        })
    }

    fn set_proxy_own_keys_complete(
        &mut self,
        state: GcRef<PendingProxyOwnKeys>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_proxy_own_keys)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .complete = true;
                Ok::<(), ExecutionError>(())
            })
        })
    }

    fn push_proxy_own_keys_continuation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingProxyOwnKeys>,
        stage: ProxyOwnKeysStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_own_keys(
                site,
                ProxyOwnKeysMode::Internal,
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

    fn proxy_own_keys_to_length(&mut self, value: Value) -> Result<u32, ExecutionError> {
        let number = self.convert_to_number(value)?;
        let number = number
            .as_i32()
            .map(f64::from)
            .or_else(|| number.as_f64())
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if number >= f64::from(u32::MAX) {
            return Err(ExecutionError::ArrayLengthOverflow);
        }
        Ok(number.floor() as u32)
    }
}

#[inline(always)]
fn proxy_own_keys_membership_contains(table: &[u64], key: PropertyKey) -> bool {
    let identity = proxy_own_keys_identity(key);
    let mut slot = proxy_own_keys_membership_slot(identity, table.len());
    loop {
        match table[slot] {
            0 => return false,
            occupied if occupied == identity => return true,
            _ => slot = (slot + 1) & (table.len() - 1),
        }
    }
}

#[inline(always)]
fn proxy_own_keys_membership_insert(
    table: &mut [u64],
    key: PropertyKey,
) -> Result<(), ExecutionError> {
    let identity = proxy_own_keys_identity(key);
    let mut slot = proxy_own_keys_membership_slot(identity, table.len());
    loop {
        match table[slot] {
            0 => {
                table[slot] = identity;
                return Ok(());
            }
            occupied if occupied == identity => {
                return Err(ExecutionError::ProxyInvariantViolation);
            }
            _ => slot = (slot + 1) & (table.len() - 1),
        }
    }
}

#[inline(always)]
const fn proxy_own_keys_membership_slot(identity: u64, length: usize) -> usize {
    let mixed = identity.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (mixed as usize) & (length - 1)
}

#[inline(always)]
const fn proxy_own_keys_identity(key: PropertyKey) -> u64 {
    match key {
        PropertyKey::Atom(atom) => ((atom.index() as u64 + 1) << 2) | 1,
        PropertyKey::Symbol(symbol) => ((symbol.serial() as u64) << 2) | 2,
        PropertyKey::Private(symbol) => ((symbol.serial() as u64) << 2) | 3,
    }
}
