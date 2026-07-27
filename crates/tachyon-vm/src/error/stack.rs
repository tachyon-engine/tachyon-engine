//! Proposal `Error.prototype.stack` accessor semantics.

use super::*;

impl Isolate {
    /// Implements the proposal getter without observing ordinary properties on the Error.
    pub(crate) fn error_stack_getter(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let receiver = site.this_value;
        if !self.is_object_value(receiver) {
            let realm = self.realm_for_callable(site.callee)?;
            return Err(self.error_stack_type_error(realm)?);
        }
        let Some(kind) = self.native_error_kind(receiver)? else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        self.allocate_runtime_string(
            JsString::try_from_latin1(kind.as_str().as_bytes())
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Begins SetterThatIgnoresPrototypeProperties with its realm-local home object.
    pub(crate) fn begin_error_stack_setter(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let home_realm = self.realm_for_callable(site.callee)?;
        let receiver = site.this_value;
        if !self.is_object_value(receiver) {
            return Err(self.error_stack_type_error(home_realm)?);
        }
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_string_value(value) {
            return Err(self.error_stack_type_error(home_realm)?);
        }
        let home = self
            .error_prototype_for_realm(home_realm)
            .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?;
        if receiver == home {
            return Err(self.error_stack_type_error(home_realm)?);
        }
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let stack = self.intern_intrinsic_name(b"stack")?;
        if self.is_proxy_value(receiver) {
            return self.dispatch_error_stack_get_own(continuation_site, receiver, value, stack);
        }
        if self
            .complete_own_property_descriptor(receiver, stack)?
            .is_some()
        {
            self.dispatch_error_stack_set(continuation_site, receiver, value, stack)
        } else {
            self.define_error_stack_data(receiver, value, stack)?;
            self.write(
                continuation_site.caller_base,
                continuation_site.destination,
                Value::from_immediate(Immediate::Undefined),
            )
        }
    }

    /// Resumes one Proxy-aware get-own, define, or set step of the stack setter.
    pub(crate) fn resume_error_stack_setter(
        &mut self,
        continuation: NativeContinuation,
        stage: ErrorStackSetterStage,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = continuation.first();
        let value = continuation.second();
        let stack = self.intern_intrinsic_name(b"stack")?;
        match stage {
            ErrorStackSetterStage::GetOwn => {
                if self.is_truthy_value(result)? {
                    self.dispatch_error_stack_set(continuation.site(), receiver, value, stack)
                } else {
                    self.dispatch_error_stack_define(continuation.site(), receiver, value, stack)
                }
            }
            ErrorStackSetterStage::Define | ErrorStackSetterStage::Set => self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_immediate(Immediate::Undefined),
            ),
        }
    }

    /// Creates proposal-mandated TypeError values in the accessor's defining Realm.
    fn error_stack_type_error(&mut self, realm: RealmId) -> Result<ExecutionError, ExecutionError> {
        self.create_native_error_in_realm(NativeErrorKind::Type, None, realm)
            .map(ExecutionError::HostThrown)
    }

    /// Returns the Error prototype belonging to a callable's defining Realm.
    fn error_prototype_for_realm(&self, realm: RealmId) -> Option<Value> {
        let realm = if realm == self.active_realm {
            Some(&self.realm)
        } else {
            self.inactive_realms
                .iter()
                .find_map(|(id, candidate)| (*id == realm).then_some(candidate))
        }?;
        realm.error_intrinsics.get(NativeErrorKind::Error).prototype
    }

    /// Runs Proxy [[GetOwnProperty]] and preserves the setter operands across callbacks.
    fn dispatch_error_stack_get_own(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
        stack: AtomId,
    ) -> Result<(), ExecutionError> {
        let key = self.atom_string_value(stack)?;
        self.push_error_stack_parent(site, ErrorStackSetterStage::GetOwn, receiver, value)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_get_own(site, receiver, key, ProxyGetOwnMode::HasOwn);
        self.finish_immediate_error_stack_dispatch(frame_depth, outcome)
    }

    /// Runs CreateDataPropertyOrThrow through Proxy [[DefineOwnProperty]].
    fn dispatch_error_stack_define(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
        stack: AtomId,
    ) -> Result<(), ExecutionError> {
        let descriptor = PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        });
        self.push_error_stack_parent(site, ErrorStackSetterStage::Define, receiver, value)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_define(
            site,
            receiver,
            PropertyKey::Atom(stack),
            descriptor,
            ProxyDefineMode::Object,
        );
        self.finish_immediate_error_stack_dispatch(frame_depth, outcome)
    }

    /// Runs Set with Throw=true on a receiver known to have an own stack property.
    fn dispatch_error_stack_set(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
        stack: AtomId,
    ) -> Result<(), ExecutionError> {
        self.push_error_stack_parent(site, ErrorStackSetterStage::Set, receiver, value)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            PropertyKey::Atom(stack),
            value,
            ProxySetMode::ObjectAssign,
        );
        self.finish_immediate_error_stack_dispatch(frame_depth, outcome)
    }

    /// Pops and resumes the parent only when a nested dispatcher completed synchronously.
    fn finish_immediate_error_stack_dispatch(
        &mut self,
        frame_depth: usize,
        outcome: Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<(), ExecutionError> {
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        debug_assert!(outcome.is_none());
        let continuation = self.pop_native_continuation()?;
        let result = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::ErrorStackSetter(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_error_stack_setter(continuation, stage, result)
    }

    /// Publishes the setter operands as precise roots before entering user code.
    fn push_error_stack_parent(
        &mut self,
        site: NativeContinuationSite,
        stage: ErrorStackSetterStage,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::error_stack_setter(
                site, stage, receiver, value,
            ))
            .map_err(Self::completion_stack_error)
    }

    /// Defines the exact CreateDataProperty descriptor on an ordinary receiver.
    fn define_error_stack_data(
        &mut self,
        receiver: Value,
        value: Value,
        stack: AtomId,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            receiver,
            stack,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            },
        )
    }
}
