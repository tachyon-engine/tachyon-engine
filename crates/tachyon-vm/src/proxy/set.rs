//! Proxy `[[Set]]` dispatch and the four-argument trap continuation.

use super::*;

const SET_TARGET: usize = 0;
const SET_KEY: usize = 1;
const SET_VALUE: usize = 2;
const SET_RECEIVER: usize = 3;
pub(crate) const SET_PROXY: usize = 4;

impl Isolate {
    /// Routes an assignment through Proxy `[[Set]]` while leaving ordinary writes unchanged.
    pub(crate) fn dispatch_proxy_aware_property_write(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        receiver: Value,
        key: PropertyKey,
        value: Value,
        mode: ProxySetMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if self.is_proxy_value(target) {
            let key_value = match key {
                PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
                PropertyKey::Symbol(symbol) => symbol.value(),
                PropertyKey::Private(_) => return Err(ExecutionError::PrivatePropertyKeyEscaped),
            };
            return self.dispatch_proxy_set(site, target, key_value, value, receiver, mode);
        }
        let result = if mode == ProxySetMode::Assignment {
            self.resolve_property_write(receiver, key, value)?
        } else {
            self.resolve_reflect_property_write(target, receiver, key, value)?
        };
        self.finish_proxy_set_result(site, mode, receiver, value, result)
    }

    /// Performs trap lookup and publishes a state object before any JavaScript callback runs.
    fn dispatch_proxy_set(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        key: Value,
        value: Value,
        receiver: Value,
        mode: ProxySetMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let snapshot = self.proxy_snapshot(proxy)?;
        if snapshot.handler.as_immediate() == Some(Immediate::Null) {
            return Err(ExecutionError::ProxyRevoked);
        }
        let trap_name = self.intern_intrinsic_name(b"set")?;
        let state = self.allocate_proxy_set_state(snapshot.target, key, value, receiver, proxy)?;
        match self.resolve_property_read(snapshot.handler, trap_name.into())? {
            PropertyRead::Missing => self.forward_proxy_set(site, state, mode),
            PropertyRead::Data(trap) => self.continue_proxy_set_lookup(site, mode, state, trap),
            PropertyRead::Accessor(getter)
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.forward_proxy_set(site, state, mode)
            }
            PropertyRead::Accessor(getter) => self.dispatch_property_callback(
                NativeContinuation::proxy_set(
                    site,
                    mode,
                    ProxySetStage::TrapGetter,
                    Value::from_heap_ref(state.raw()),
                    snapshot.handler,
                ),
                getter,
            ),
        }
    }

    /// Applies `GetMethod` and invokes the set trap with `(target, key, value, receiver)`.
    fn continue_proxy_set_lookup(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetMode,
        state: GcRef<NativeCallState>,
        trap: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if matches!(
            trap.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return self.forward_proxy_set(site, state, mode);
        }
        self.resolve_function_object(trap)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_set(
                site,
                mode,
                ProxySetStage::TrapCall,
                Value::from_heap_ref(state.raw()),
                trap,
            ),
            trap,
        )
    }

    /// Executes the target's ordinary `[[Set]]` when the handler has no trap.
    fn forward_proxy_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        mode: ProxySetMode,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let key = self.property_key(pending.values[SET_KEY])?;
        let resolution = self.resolve_reflect_property_write_until_proxy(
            pending.values[SET_TARGET],
            pending.values[SET_RECEIVER],
            key,
            pending.values[SET_VALUE],
        );
        let resolution = match resolution {
            Ok(resolution) => resolution,
            Err(ExecutionError::NotObject(receiver))
                if receiver == pending.values[SET_RECEIVER]
                    && self.proxy_receiver_has_no_descriptor_write_traps(receiver)? =>
            {
                let success = match self.set_own_data_property(
                    pending.values[SET_TARGET],
                    key,
                    pending.values[SET_VALUE],
                ) {
                    Ok(()) => true,
                    Err(
                        ExecutionError::NonExtensibleObject(_)
                        | ExecutionError::ReadOnlyProperty(_),
                    ) => false,
                    Err(error) => return Err(error),
                };
                PropertyWriteResolution::Write(PropertyWrite::Complete(success))
            }
            Err(error) => return Err(error),
        };
        match resolution {
            PropertyWriteResolution::Proxy(proxy) => self.dispatch_proxy_set(
                site,
                proxy,
                pending.values[SET_KEY],
                pending.values[SET_VALUE],
                pending.values[SET_RECEIVER],
                mode,
            ),
            PropertyWriteResolution::Write(result) => self.finish_proxy_set_result(
                site,
                mode,
                pending.values[SET_RECEIVER],
                pending.values[SET_VALUE],
                result,
            ),
        }
    }

    /// Detects the side-effect-free receiver case where Proxy descriptor operations forward.
    fn proxy_receiver_has_no_descriptor_write_traps(
        &mut self,
        receiver: Value,
    ) -> Result<bool, ExecutionError> {
        let handler = self.proxy_snapshot(receiver)?.handler;
        for name in [
            b"getOwnPropertyDescriptor".as_slice(),
            b"defineProperty".as_slice(),
        ] {
            let atom = self.intern_intrinsic_name(name)?;
            match self.resolve_property_read(handler, atom.into())? {
                PropertyRead::Missing => {}
                PropertyRead::Data(value)
                    if matches!(
                        value.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) => {}
                PropertyRead::Data(_) | PropertyRead::Accessor(_) => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Resumes a trap getter/call and maps the result to assignment or Reflect semantics.
    pub(crate) fn resume_proxy_set(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxySetMode,
        stage: ProxySetStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ProxySetStage::TrapGetter => {
                self.continue_proxy_set_lookup(continuation.site(), mode, state, value)
            }
            ProxySetStage::TrapCall => {
                let pending = self.native_call_state_snapshot(state)?;
                let success = self.is_truthy_value(value)?;
                if success {
                    self.validate_proxy_set_result(pending)?;
                }
                let result = PropertyWrite::Complete(success);
                self.finish_proxy_set_result(
                    continuation.site(),
                    mode,
                    pending.values[SET_RECEIVER],
                    pending.values[SET_VALUE],
                    result,
                )
            }
        }
    }

    /// Enforces the frozen data and setter-less accessor restrictions on a truthy trap result.
    fn validate_proxy_set_result(
        &mut self,
        pending: NativeCallState,
    ) -> Result<(), ExecutionError> {
        let key = self.property_key(pending.values[SET_KEY])?;
        let Some(descriptor) =
            self.complete_own_property_descriptor(pending.values[SET_TARGET], key)?
        else {
            return Ok(());
        };
        match descriptor {
            PropertyDescriptor::Data(descriptor)
                if descriptor.configurable == Some(false) && descriptor.writable == Some(false) =>
            {
                let current = descriptor
                    .value
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                if !self.same_value(pending.values[SET_VALUE], current)? {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
            }
            PropertyDescriptor::Accessor(descriptor)
                if descriptor.configurable == Some(false)
                    && descriptor.setter.is_none_or(|setter| {
                        setter.as_immediate() == Some(Immediate::Undefined)
                    }) =>
            {
                return Err(ExecutionError::ProxyInvariantViolation);
            }
            _ => {}
        }
        Ok(())
    }

    /// Applies the public boolean/strict assignment result after Proxy trap completion.
    fn finish_proxy_set_result(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetMode,
        receiver: Value,
        value: Value,
        result: PropertyWrite,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match result {
            PropertyWrite::Setter(callee) => self.dispatch_property_callback(
                if mode == ProxySetMode::Assignment {
                    NativeContinuation::property_set(site, receiver, value)
                } else {
                    NativeContinuation::reflect_property_set(site, receiver, value)
                },
                callee,
            ),
            PropertyWrite::Complete(success) => {
                if mode == ProxySetMode::Reflect {
                    self.write(site.caller_base, site.destination, boolean_value(success))?;
                    return Ok(None);
                }
                self.write(site.caller_base, site.destination, value)?;
                self.finish_property_write(receiver, success)?;
                Ok(None)
            }
        }
    }

    /// Allocates the traced target/key/value/receiver tuple used by every Proxy set stage.
    fn allocate_proxy_set_state(
        &mut self,
        target: Value,
        key: Value,
        value: Value,
        receiver: Value,
        proxy: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let values = [target, key, value, receiver, proxy];
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
                    count: 4,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }
}
