//! Proxy `[[Set]]` dispatch and the four-argument trap continuation.

use super::*;
use crate::property::ArrayLengthSetConsumer;

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
        let result = if matches!(mode, ProxySetMode::Assignment | ProxySetMode::ObjectAssign) {
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
        let state = self.allocate_proxy_set_state(snapshot.target, key, value, receiver, proxy)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let trap_name = self.intern_intrinsic_name(b"set")?;
        if self.is_proxy_value(snapshot.handler) {
            let state_value = self.read(site.caller_base, site.destination)?;
            let state = self.native_call_state_reference(state_value)?;
            let active_proxy = self.native_call_state_snapshot(state)?.values[SET_PROXY];
            let handler = self.proxy_snapshot(active_proxy)?.handler;
            return self.dispatch_proxy_set_handler_get(
                site,
                mode,
                state,
                handler,
                trap_name.into(),
            );
        }
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

    /// Reads `handler.set` through nested Proxy layers before applying GetMethod.
    fn dispatch_proxy_set_handler_get(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetMode,
        state: GcRef<NativeCallState>,
        handler: Value,
        key: PropertyKey,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_set_parent(site, mode, state, ProxySetStage::TrapGetter, handler)?;
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
        self.resume_proxy_set(continuation, mode, ProxySetStage::TrapGetter, trap)
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
                if receiver == pending.values[SET_RECEIVER] && self.is_proxy_value(receiver) =>
            {
                return self.dispatch_proxy_receiver_write(site, mode, state);
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

    /// Starts receiver `[[GetOwnProperty]]` before OrdinarySet chooses a define operation.
    fn dispatch_proxy_receiver_write(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetMode,
        state: GcRef<NativeCallState>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let receiver = pending.values[SET_RECEIVER];
        let key = pending.values[SET_KEY];
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_set_parent(site, mode, state, ProxySetStage::ReceiverGetOwn, receiver)?;
        let outcome =
            match self.dispatch_proxy_get_own(site, receiver, key, ProxyGetOwnMode::SetReceiver) {
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
        self.resume_proxy_set(
            continuation,
            mode,
            ProxySetStage::ReceiverGetOwn,
            descriptor,
        )
    }

    /// Converts receiver descriptor state into either a failed write or a Proxy define call.
    fn continue_proxy_receiver_write(
        &mut self,
        continuation: NativeContinuation,
        mode: ProxySetMode,
        state: GcRef<NativeCallState>,
        descriptor_value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.update_proxy_state_value(state, SET_PROXY, descriptor_value)?;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let pending = self.native_call_state_snapshot(state)?;
        let descriptor_value = pending.values[SET_PROXY];
        let descriptor_absent = descriptor_value.as_immediate() == Some(Immediate::Undefined);
        if descriptor_value.as_immediate() == Some(Immediate::False) {
            return self.finish_proxy_set_result(
                continuation.site(),
                mode,
                pending.values[SET_RECEIVER],
                pending.values[SET_VALUE],
                PropertyWrite::Complete(false),
            );
        }
        if descriptor_value.as_immediate() != Some(Immediate::True) && !descriptor_absent {
            let descriptor = self.parse_property_descriptor(descriptor_value)?;
            match descriptor {
                PropertyDescriptor::Accessor(_) => {
                    return self.finish_proxy_set_result(
                        continuation.site(),
                        mode,
                        pending.values[SET_RECEIVER],
                        pending.values[SET_VALUE],
                        PropertyWrite::Complete(false),
                    );
                }
                PropertyDescriptor::Data(data) if data.writable == Some(false) => {
                    return self.finish_proxy_set_result(
                        continuation.site(),
                        mode,
                        pending.values[SET_RECEIVER],
                        pending.values[SET_VALUE],
                        PropertyWrite::Complete(false),
                    );
                }
                PropertyDescriptor::Data(_) | PropertyDescriptor::Generic(_) => {}
            }
        }
        let descriptor = PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(pending.values[SET_VALUE]),
            writable: descriptor_absent.then_some(true),
            enumerable: descriptor_absent.then_some(true),
            configurable: descriptor_absent.then_some(true),
        });
        self.dispatch_proxy_receiver_define(continuation.site(), mode, state, descriptor)
    }

    /// Defines the value on a Proxy receiver through the existing descriptor/invariant dispatcher.
    fn dispatch_proxy_receiver_define(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetMode,
        state: GcRef<NativeCallState>,
        descriptor: PropertyDescriptor,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let receiver = pending.values[SET_RECEIVER];
        let key = self.property_key(pending.values[SET_KEY])?;
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_proxy_set_parent(site, mode, state, ProxySetStage::ReceiverDefine, receiver)?;
        let outcome = match self.dispatch_proxy_define(
            site,
            receiver,
            key,
            descriptor,
            ProxyDefineMode::Reflect,
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
        let result = self.read(site.caller_base, site.destination)?;
        self.resume_proxy_set(continuation, mode, ProxySetStage::ReceiverDefine, result)
    }

    /// Pushes one traced ProxySet parent around nested receiver operations.
    fn push_proxy_set_parent(
        &mut self,
        site: NativeContinuationSite,
        mode: ProxySetMode,
        state: GcRef<NativeCallState>,
        stage: ProxySetStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::proxy_set(
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
            ProxySetStage::ReceiverGetOwn => {
                self.continue_proxy_receiver_write(continuation, mode, state, value)
            }
            ProxySetStage::ReceiverDefine => {
                let pending = self.native_call_state_snapshot(state)?;
                let success = self.is_truthy_value(value)?;
                self.finish_proxy_set_result(
                    continuation.site(),
                    mode,
                    pending.values[SET_RECEIVER],
                    pending.values[SET_VALUE],
                    PropertyWrite::Complete(success),
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
            PropertyWrite::Setter(callee) => {
                self.write(site.caller_base, site.destination, value)?;
                self.dispatch_property_callback(
                    if matches!(mode, ProxySetMode::Assignment | ProxySetMode::ObjectAssign) {
                        NativeContinuation::property_set(site, receiver, value)
                    } else {
                        NativeContinuation::reflect_property_set(site, receiver, value)
                    },
                    callee,
                )
            }
            PropertyWrite::Complete(success) => {
                if mode == ProxySetMode::Reflect {
                    self.write(site.caller_base, site.destination, boolean_value(success))?;
                    return Ok(None);
                }
                self.write(site.caller_base, site.destination, value)?;
                if mode == ProxySetMode::ObjectAssign && !success {
                    return Err(ExecutionError::ReadOnlyProperty(receiver));
                }
                self.finish_property_write(receiver, success)?;
                Ok(None)
            }
            PropertyWrite::ArrayLength => self
                .dispatch_array_length_property_set(
                    site,
                    receiver,
                    value,
                    if mode == ProxySetMode::ObjectAssign {
                        ArrayLengthSetConsumer::ProxyObjectAssign
                    } else if mode == ProxySetMode::Reflect {
                        ArrayLengthSetConsumer::Reflect
                    } else {
                        ArrayLengthSetConsumer::Assignment
                    },
                )
                .map(|()| None),
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
                    count: 4,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }
}
