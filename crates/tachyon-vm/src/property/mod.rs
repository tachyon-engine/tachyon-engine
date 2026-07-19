//! Ordinary property lookup, data descriptors, and intrinsic property atoms.

mod function;
mod storage;

use super::*;

impl Isolate {
    #[inline(always)]
    pub(crate) fn safe_integer_property_atom(
        &mut self,
        index: u64,
    ) -> Result<AtomId, ExecutionError> {
        debug_assert!(index <= MAX_SAFE_INTEGER);
        if let Ok(integer) = i32::try_from(index) {
            return self.property_key_atom(Value::from_i32(integer));
        }
        self.property_key_atom(Value::from_f64(index as f64))
    }

    /// Walks ordinary prototype links without allocating or invoking accessor/exotic behavior.
    pub(crate) fn get_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<Option<Value>, ExecutionError> {
        let key = key.into();
        let mut current = if numeric_value(receiver).is_some() {
            self.realm
                .number_prototype
                .expect("Number prototype initializes before property access")
        } else {
            receiver
        };
        loop {
            let (_, snapshot) = self.object_snapshot(current)?;
            if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
                if let Some(value) = self.property_value_from_snapshot(snapshot, property)? {
                    return Ok(Some(value));
                }
            } else {
                if let Some(value) = self.function_metadata_property(current, key)? {
                    return Ok(Some(value));
                }
                if self.is_function_prototype_property(current, key) {
                    self.intrinsic_property_atoms.prototype = key.atom();
                    return self.ensure_function_prototype(current).map(Some);
                }
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(None);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Reads only an object's own data slot, excluding inherited prototype properties.
    pub(crate) fn has_own_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<bool, ExecutionError> {
        Ok(self
            .own_data_property_with_attributes(receiver, key)?
            .is_some())
    }

    /// Resolves virtual function fields and ordinary own slots with their exact data flags.
    pub(crate) fn own_data_property_with_attributes(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<Option<(Value, PropertyAttributes)>, ExecutionError> {
        let key = key.into();
        let (_, snapshot) = self.object_snapshot(receiver)?;
        if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
            return Ok(self
                .property_value_from_snapshot(snapshot, property)?
                .map(|value| (value, property.attributes)));
        }
        if let Some(value) = self.function_metadata_property(receiver, key)? {
            return Ok(Some((value, PropertyAttributes::data(false, false, true))));
        }
        if self.is_function_prototype_property(receiver, key) {
            self.intrinsic_property_atoms.prototype = key.atom();
            let value = self.ensure_function_prototype(receiver)?;
            return Ok(Some((value, PropertyAttributes::data(true, false, false))));
        }
        Ok(None)
    }

    /// Parses the supported data fields while preserving absent versus present-undefined.
    pub(crate) fn parse_data_property_descriptor(
        &mut self,
        descriptor: Value,
    ) -> Result<DataPropertyDescriptor, ExecutionError> {
        if !self.is_object_value(descriptor) {
            return Err(ExecutionError::NotObject(descriptor));
        }
        let value_atom = self.intern_intrinsic_name(b"value")?;
        let writable_atom = self.intern_intrinsic_name(b"writable")?;
        let enumerable_atom = self.intern_intrinsic_name(b"enumerable")?;
        let configurable_atom = self.intern_intrinsic_name(b"configurable")?;
        let get_atom = self.intern_intrinsic_name(b"get")?;
        let set_atom = self.intern_intrinsic_name(b"set")?;
        let value = self.get_data_property(descriptor, value_atom)?;
        let writable = self
            .get_data_property(descriptor, writable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let enumerable = self
            .get_data_property(descriptor, enumerable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let configurable = self
            .get_data_property(descriptor, configurable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let getter = self.get_data_property(descriptor, get_atom)?;
        let setter = self.get_data_property(descriptor, set_atom)?;
        if getter.is_some() || setter.is_some() {
            return Err(ExecutionError::UnsupportedAccessorDescriptor);
        }
        Ok(DataPropertyDescriptor {
            value,
            writable,
            enumerable,
            configurable,
        })
    }

    /// Defines one spec-facing intrinsic field with non-enumerable builtin attributes.
    pub(crate) fn set_intrinsic_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
        value: Value,
        configurable: bool,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            receiver,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(configurable),
            },
        )
    }

    /// Defines one non-writable, non-enumerable, non-configurable intrinsic constant.
    pub(crate) fn set_intrinsic_constant_property(
        &mut self,
        receiver: Value,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            receiver,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(false),
                enumerable: Some(false),
                configurable: Some(false),
            },
        )
    }

    /// Implements ValidateAndApplyPropertyDescriptor for ordinary data properties.
    pub(crate) fn define_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
        descriptor: DataPropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let key = key.into();
        if self.is_function_prototype_property(receiver, key) {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let property = self.shapes.lookup(snapshot.shape, key);
        if property.is_none()
            && let Some(current_value) = self.function_metadata_property(receiver, key)?
        {
            let current_attributes = PropertyAttributes::data(false, false, true);
            self.validate_data_property_redefinition(
                receiver,
                current_value,
                current_attributes,
                descriptor,
            )?;
            let attributes = PropertyAttributes::data(
                descriptor.writable.unwrap_or(false),
                descriptor.enumerable.unwrap_or(false),
                descriptor.configurable.unwrap_or(true),
            );
            return self.add_property_slot(
                object,
                snapshot,
                key,
                descriptor.value.unwrap_or(current_value),
                attributes,
            );
        }
        let current = property
            .map(|property| self.property_value_from_snapshot(snapshot, property))
            .transpose()?
            .flatten();
        let Some(current_value) = current else {
            if !snapshot.extensible {
                return Err(ExecutionError::NonExtensibleObject(receiver));
            }
            let attributes = PropertyAttributes::data(
                descriptor.writable.unwrap_or(false),
                descriptor.enumerable.unwrap_or(false),
                descriptor.configurable.unwrap_or(false),
            );
            let value = descriptor
                .value
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            if let Some(property) = property {
                let shape = self
                    .shapes
                    .transition_reconfigure(snapshot.shape, key, attributes)
                    .map_err(ExecutionError::Shape)?;
                self.update_property_slot(snapshot, key, property.slot, value)?;
                return self.set_object_shape(object, shape);
            }
            return self.add_property_slot(object, snapshot, key, value, attributes);
        };
        let property = property.expect("present property value has shape metadata");
        self.validate_data_property_redefinition(
            receiver,
            current_value,
            property.attributes,
            descriptor,
        )?;
        let attributes = PropertyAttributes::data(
            descriptor
                .writable
                .unwrap_or_else(|| property.attributes.writable()),
            descriptor
                .enumerable
                .unwrap_or_else(|| property.attributes.enumerable()),
            descriptor
                .configurable
                .unwrap_or_else(|| property.attributes.configurable()),
        );
        let shape = self
            .shapes
            .transition_reconfigure(snapshot.shape, key, attributes)
            .map_err(ExecutionError::Shape)?;
        if let Some(value) = descriptor.value {
            self.update_property_slot(snapshot, key, property.slot, value)?;
        }
        self.set_object_shape(object, shape)
    }

    /// Rejects the immutable combinations required by data descriptor compatibility.
    fn validate_data_property_redefinition(
        &mut self,
        receiver: Value,
        current_value: Value,
        current: PropertyAttributes,
        descriptor: DataPropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        if current.configurable() {
            return Ok(());
        }
        if descriptor.configurable == Some(true)
            || descriptor
                .enumerable
                .is_some_and(|enumerable| enumerable != current.enumerable())
            || (!current.writable() && descriptor.writable == Some(true))
        {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        if !current.writable()
            && let Some(value) = descriptor.value
            && !self.same_value(value, current_value)?
        {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        Ok(())
    }

    /// Populates a rooted fresh object with the four standard data descriptor fields.
    pub(crate) fn materialize_data_property_descriptor(
        &mut self,
        result: Value,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        let value_atom = self.intern_intrinsic_name(b"value")?;
        self.set_own_data_property(result, value_atom, value)?;
        let writable_atom = self.intern_intrinsic_name(b"writable")?;
        self.set_own_data_property(
            result,
            writable_atom,
            Value::from_immediate(if attributes.writable() {
                Immediate::True
            } else {
                Immediate::False
            }),
        )?;
        let enumerable_atom = self.intern_intrinsic_name(b"enumerable")?;
        self.set_own_data_property(
            result,
            enumerable_atom,
            Value::from_immediate(if attributes.enumerable() {
                Immediate::True
            } else {
                Immediate::False
            }),
        )?;
        let configurable_atom = self.intern_intrinsic_name(b"configurable")?;
        self.set_own_data_property(
            result,
            configurable_atom,
            Value::from_immediate(if attributes.configurable() {
                Immediate::True
            } else {
                Immediate::False
            }),
        )
    }

    pub(crate) fn prototype_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.prototype {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"prototype").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.prototype = Some(atom);
        Ok(atom)
    }

    pub(crate) fn constructor_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.constructor {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"constructor").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.constructor = Some(atom);
        Ok(atom)
    }

    pub(crate) fn message_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.message {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"message").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.message = Some(atom);
        Ok(atom)
    }

    pub(crate) fn name_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.name {
            return Ok(atom);
        }
        let atom = self.intern_intrinsic_name(b"name")?;
        self.intrinsic_property_atoms.name = Some(atom);
        Ok(atom)
    }

    pub(crate) fn length_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.length {
            return Ok(atom);
        }
        let atom = self.intern_intrinsic_name(b"length")?;
        self.intrinsic_property_atoms.length = Some(atom);
        Ok(atom)
    }
}
