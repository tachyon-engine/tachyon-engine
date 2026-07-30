//! Intrinsic `Iterator` constructor and proposal-defined prototype accessors.

use super::*;

impl Isolate {
    /// Returns the constructor from the accessor function's defining Realm.
    pub(crate) fn iterator_constructor_getter(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let realm = self.realm_for_callable(site.callee)?;
        self.iterator_constructor_for_realm(realm)
            .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })
    }

    /// Returns the proposal's fixed `%IteratorPrototype%` brand string.
    pub(crate) fn iterator_to_string_tag_getter(&mut self) -> Result<Value, ExecutionError> {
        self.allocate_runtime_string(
            JsString::try_from_latin1(b"Iterator").map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Begins SetterThatIgnoresPrototypeProperties for either special property.
    pub(crate) fn begin_iterator_prototype_setter(
        &mut self,
        site: &CallSite,
        key: IteratorPrototypeSetterKey,
    ) -> Result<(), ExecutionError> {
        let home_realm = self.realm_for_callable(site.callee)?;
        let receiver = site.this_value;
        if !self.is_object_value(receiver) {
            return Err(self.iterator_type_error(home_realm)?);
        }
        let home = self
            .iterator_prototype_for_realm(home_realm)
            .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?;
        if receiver == home {
            return Err(self.iterator_type_error(home_realm)?);
        }
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let property_key = self.iterator_setter_property_key(key)?;
        if self.is_proxy_value(receiver) {
            return self.dispatch_iterator_get_own(continuation_site, key, receiver, value);
        }
        if self
            .complete_own_property_descriptor(receiver, property_key)?
            .is_some()
        {
            self.dispatch_iterator_set(continuation_site, key, receiver, value, property_key)
        } else {
            self.define_iterator_data(receiver, value, property_key)?;
            self.write(
                continuation_site.caller_base,
                continuation_site.destination,
                Value::from_immediate(Immediate::Undefined),
            )
        }
    }

    /// Resumes a Proxy-aware get-own, define, or set step of the special setter.
    pub(crate) fn resume_iterator_prototype_setter(
        &mut self,
        continuation: NativeContinuation,
        key: IteratorPrototypeSetterKey,
        stage: IteratorPrototypeSetterStage,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = continuation.first();
        let value = continuation.second();
        let property_key = self.iterator_setter_property_key(key)?;
        match stage {
            IteratorPrototypeSetterStage::GetOwn => {
                if self.is_truthy_value(result)? {
                    self.dispatch_iterator_set(
                        continuation.site(),
                        key,
                        receiver,
                        value,
                        property_key,
                    )
                } else {
                    self.dispatch_iterator_define(
                        continuation.site(),
                        key,
                        receiver,
                        value,
                        property_key,
                    )
                }
            }
            IteratorPrototypeSetterStage::Define | IteratorPrototypeSetterStage::Set => self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_immediate(Immediate::Undefined),
            ),
        }
    }

    /// Implements GetPrototypeFromConstructor with the newTarget Realm's Iterator fallback.
    pub(crate) fn construct_iterator_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let prototype_atom = self.prototype_atom()?;
        let explicit = self
            .constructor_prototype_value(site.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value));
        let prototype = if let Some(prototype) = explicit {
            prototype
        } else {
            let realm = self.realm_for_callable(site.new_target)?;
            self.iterator_prototype_for_realm(realm)
                .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?
        };
        self.create_ordinary_object_with_prototype(prototype)
    }

    /// Creates a TypeError in the intrinsic accessor's defining Realm.
    pub(crate) fn iterator_type_error(
        &mut self,
        realm: RealmId,
    ) -> Result<ExecutionError, ExecutionError> {
        self.create_native_error_in_realm(NativeErrorKind::Type, None, realm)
            .map(ExecutionError::HostThrown)
    }

    /// Looks up one Realm's `%Iterator%` constructor without changing the active Realm.
    fn iterator_constructor_for_realm(&self, realm: RealmId) -> Option<Value> {
        self.realm_record(realm)?.iterator_constructor
    }

    /// Looks up one Realm's `%IteratorPrototype%` without extending isolate-wide kinds.
    fn iterator_prototype_for_realm(&self, realm: RealmId) -> Option<Value> {
        self.realm_record(realm)?.iterator_prototype
    }

    /// Returns a Realm record from either the active slot or inactive Realm table.
    fn realm_record(&self, realm: RealmId) -> Option<&Realm> {
        if realm == self.active_realm {
            Some(&self.realm)
        } else {
            self.inactive_realms
                .iter()
                .find_map(|(id, candidate)| (*id == realm).then_some(candidate))
        }
    }

    /// Resolves the compact setter key to the corresponding property identity.
    fn iterator_setter_property_key(
        &mut self,
        key: IteratorPrototypeSetterKey,
    ) -> Result<PropertyKey, ExecutionError> {
        match key {
            IteratorPrototypeSetterKey::Constructor => {
                Ok(PropertyKey::Atom(self.constructor_atom()?))
            }
            IteratorPrototypeSetterKey::ToStringTag => {
                let symbol = self
                    .agent
                    .well_known_symbols
                    .to_string_tag
                    .expect("Symbol.toStringTag initializes before Iterator");
                self.property_key(symbol)
            }
        }
    }

    /// Converts a property key into the Value form required by Proxy [[GetOwnProperty]].
    fn iterator_setter_key_value(
        &mut self,
        key: IteratorPrototypeSetterKey,
    ) -> Result<Value, ExecutionError> {
        match key {
            IteratorPrototypeSetterKey::Constructor => {
                let atom = self.constructor_atom()?;
                self.atom_string_value(atom)
            }
            IteratorPrototypeSetterKey::ToStringTag => Ok(self
                .agent
                .well_known_symbols
                .to_string_tag
                .expect("Symbol.toStringTag initializes before Iterator")),
        }
    }

    /// Runs Proxy [[GetOwnProperty]] while retaining both setter operands.
    fn dispatch_iterator_get_own(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let key_value = self.iterator_setter_key_value(key)?;
        self.push_iterator_setter_parent(
            site,
            key,
            IteratorPrototypeSetterStage::GetOwn,
            receiver,
            value,
        )?;
        let frame_depth = self.fiber.frames.len();
        let outcome =
            self.dispatch_proxy_get_own(site, receiver, key_value, ProxyGetOwnMode::HasOwn);
        self.finish_immediate_iterator_dispatch(frame_depth, outcome)
    }

    /// Runs CreateDataPropertyOrThrow through Proxy [[DefineOwnProperty]].
    fn dispatch_iterator_define(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        receiver: Value,
        value: Value,
        property_key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let descriptor = PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        });
        self.push_iterator_setter_parent(
            site,
            key,
            IteratorPrototypeSetterStage::Define,
            receiver,
            value,
        )?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_define(
            site,
            receiver,
            property_key,
            descriptor,
            ProxyDefineMode::Object,
        );
        self.finish_immediate_iterator_dispatch(frame_depth, outcome)
    }

    /// Runs Set with Throw=true on a receiver known to own the special property.
    fn dispatch_iterator_set(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        receiver: Value,
        value: Value,
        property_key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        self.push_iterator_setter_parent(
            site,
            key,
            IteratorPrototypeSetterStage::Set,
            receiver,
            value,
        )?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            property_key,
            value,
            ProxySetMode::ObjectAssign,
        );
        self.finish_immediate_iterator_dispatch(frame_depth, outcome)
    }

    /// Pops and resumes the parent when a nested dispatcher completed synchronously.
    fn finish_immediate_iterator_dispatch(
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
        let NativeContinuationKind::IteratorPrototypeSetter { key, stage } = continuation.kind()
        else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_iterator_prototype_setter(continuation, key, stage, result)
    }

    /// Publishes setter operands as precise roots before entering user code.
    fn push_iterator_setter_parent(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        stage: IteratorPrototypeSetterStage,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_prototype_setter(
                site, key, stage, receiver, value,
            ))
            .map_err(Self::completion_stack_error)
    }

    /// Defines the mandated writable, enumerable, configurable data property.
    fn define_iterator_data(
        &mut self,
        receiver: Value,
        value: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            receiver,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            },
        )
    }
}
