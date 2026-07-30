//! Closed property descriptors and ordinary ValidateAndApplyPropertyDescriptor semantics.

use super::{super::*, accessor::StoredProperty};

impl PropertyDescriptor {
    pub(crate) fn enumerable(self) -> Option<bool> {
        match self {
            Self::Generic(descriptor) => descriptor.enumerable,
            Self::Data(descriptor) => descriptor.enumerable,
            Self::Accessor(descriptor) => descriptor.enumerable,
        }
    }

    pub(crate) fn configurable(self) -> Option<bool> {
        match self {
            Self::Generic(descriptor) => descriptor.configurable,
            Self::Data(descriptor) => descriptor.configurable,
            Self::Accessor(descriptor) => descriptor.configurable,
        }
    }

    /// Applies CompletePropertyDescriptor defaults before Proxy invariants or public materialization.
    pub(crate) fn complete(self) -> Self {
        let undefined = Value::from_immediate(Immediate::Undefined);
        match self {
            Self::Generic(descriptor) => Self::Data(DataPropertyDescriptor {
                value: Some(undefined),
                writable: Some(false),
                enumerable: Some(descriptor.enumerable.unwrap_or(false)),
                configurable: Some(descriptor.configurable.unwrap_or(false)),
            }),
            Self::Data(descriptor) => Self::Data(DataPropertyDescriptor {
                value: Some(descriptor.value.unwrap_or(undefined)),
                writable: Some(descriptor.writable.unwrap_or(false)),
                enumerable: Some(descriptor.enumerable.unwrap_or(false)),
                configurable: Some(descriptor.configurable.unwrap_or(false)),
            }),
            Self::Accessor(descriptor) => Self::Accessor(AccessorPropertyDescriptor {
                getter: Some(descriptor.getter.unwrap_or(undefined)),
                setter: Some(descriptor.setter.unwrap_or(undefined)),
                enumerable: Some(descriptor.enumerable.unwrap_or(false)),
                configurable: Some(descriptor.configurable.unwrap_or(false)),
            }),
        }
    }

    fn requested_kind(self) -> Option<PropertyKind> {
        match self {
            Self::Generic(_) => None,
            Self::Data(_) => Some(PropertyKind::Data),
            Self::Accessor(_) => Some(PropertyKind::Accessor),
        }
    }
}

impl Isolate {
    /// Converts one descriptor object into a closed generic, data, or accessor state.
    pub(crate) fn parse_property_descriptor(
        &mut self,
        descriptor: Value,
    ) -> Result<PropertyDescriptor, ExecutionError> {
        if !self.is_object_value(descriptor) {
            return Err(ExecutionError::NotObject(descriptor));
        }
        let enumerable_atom = self.intern_intrinsic_name(b"enumerable")?;
        let configurable_atom = self.intern_intrinsic_name(b"configurable")?;
        let value_atom = self.intern_intrinsic_name(b"value")?;
        let writable_atom = self.intern_intrinsic_name(b"writable")?;
        let get_atom = self.intern_intrinsic_name(b"get")?;
        let set_atom = self.intern_intrinsic_name(b"set")?;
        let enumerable = self
            .get_data_property(descriptor, enumerable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let configurable = self
            .get_data_property(descriptor, configurable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let value = self.get_data_property(descriptor, value_atom)?;
        let writable = self
            .get_data_property(descriptor, writable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let getter = self.get_data_property(descriptor, get_atom)?;
        let setter = self.get_data_property(descriptor, set_atom)?;
        let has_data_fields = value.is_some() || writable.is_some();
        let has_accessor_fields = getter.is_some() || setter.is_some();
        if has_data_fields && has_accessor_fields {
            return Err(ExecutionError::InvalidPropertyDescriptor(descriptor));
        }
        if has_accessor_fields {
            if let Some(getter) = getter {
                self.validate_accessor_callable(getter)?;
            }
            if let Some(setter) = setter {
                self.validate_accessor_callable(setter)?;
            }
            return Ok(PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter,
                setter,
                enumerable,
                configurable,
            }));
        }
        if has_data_fields {
            return Ok(PropertyDescriptor::Data(DataPropertyDescriptor {
                value,
                writable,
                enumerable,
                configurable,
            }));
        }
        Ok(PropertyDescriptor::Generic(GenericPropertyDescriptor {
            enumerable,
            configurable,
        }))
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

    /// Defines one read-only spec-facing intrinsic field with builtin attributes.
    pub(crate) fn set_intrinsic_readonly_data_property(
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
                writable: Some(false),
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

    /// Thin data-descriptor adapter retained for existing callers during the unified migration.
    pub(crate) fn define_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
        descriptor: impl Into<PropertyDescriptor>,
    ) -> Result<(), ExecutionError> {
        self.define_property(receiver, key.into(), descriptor.into())
    }

    /// Validates one complete ordinary mutation before publishing its kind, flags, or payload.
    pub(crate) fn define_property(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        if self.is_module_namespace_value(receiver)
            && self.module_namespace_has_export(receiver, key)?
        {
            if self.define_module_namespace_property(receiver, key, descriptor)? {
                return Ok(());
            }
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        if self.is_typed_array_value(receiver) {
            match self.typed_array_index(key)? {
                crate::builtins::typed_array::TypedArrayIndex::NonNumeric => {}
                crate::builtins::typed_array::TypedArrayIndex::Invalid => {
                    return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                }
                crate::builtins::typed_array::TypedArrayIndex::Valid(index) => {
                    if index >= self.typed_array_snapshot(receiver)?.length {
                        return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                    }
                    let PropertyDescriptor::Data(data) = descriptor else {
                        return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                    };
                    if data.configurable == Some(false)
                        || data.enumerable == Some(false)
                        || data.writable == Some(false)
                    {
                        return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                    }
                    if let Some(value) = data.value
                        && self.typed_array_index_set(receiver, key, value)? != Some(true)
                    {
                        return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                    }
                    return Ok(());
                }
            }
        }
        let mapped_value_before_freeze = match &descriptor {
            PropertyDescriptor::Data(data)
                if data.writable == Some(false) && data.value.is_none() =>
            {
                self.mapped_argument_value(receiver, key)?
            }
            _ => None,
        };
        let descriptor = match descriptor {
            PropertyDescriptor::Data(mut data) => {
                if data.value.is_none() {
                    data.value = mapped_value_before_freeze;
                }
                PropertyDescriptor::Data(data)
            }
            descriptor => descriptor,
        };
        if let PropertyDescriptor::Data(data) = &descriptor
            && let Some(value) = data.value
        {
            self.sync_mapped_argument(receiver, key, value)?;
        }
        if let PropertyDescriptor::Accessor(accessor) = descriptor {
            if let Some(getter) = accessor.getter {
                self.validate_accessor_callable(getter)?;
            }
            if let Some(setter) = accessor.setter {
                self.validate_accessor_callable(setter)?;
            }
        }
        if self.is_string_wrapper(receiver) {
            let length = self.length_atom()?;
            let existing_index = key.atom().is_some_and(|atom| {
                self.atoms
                    .get(atom)
                    .and_then(|name| crate::property::keys::array_index(name.as_view()))
                    .is_some_and(|index| {
                        self.string_index_value(receiver, index as usize)
                            .ok()
                            .flatten()
                            .is_some()
                    })
            });
            if key == PropertyKey::Atom(length) || existing_index {
                return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
            }
        }
        if self.is_array_value(receiver)? {
            if key == PropertyKey::Atom(self.length_atom()?) {
                return self.array_set_length_descriptor(receiver, descriptor);
            }
            if let Some(index) = self.array_property_index(key) {
                self.guard_array_index_growth(receiver, index)?;
            }
        }
        if self.is_function_prototype_property(receiver, key) {
            if let PropertyDescriptor::Data(data) = descriptor
                && data.writable.is_none()
                && data.enumerable.is_none()
                && data.configurable.is_none()
                && let Some(value) = data.value
            {
                self.intrinsic_property_atoms.prototype = key.atom();
                return self.set_function_prototype(receiver, value);
            }
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        if let Some(current_value) = self.dense_array_value(receiver, key)? {
            match descriptor {
                PropertyDescriptor::Data(data)
                    if data.writable != Some(false)
                        && data.enumerable != Some(false)
                        && data.configurable != Some(false) =>
                {
                    if let Some(value) = data.value {
                        let index = self
                            .array_property_index(key)
                            .expect("dense properties have canonical Array indices");
                        self.set_dense_array_value(receiver, index, value)?;
                    }
                    return Ok(());
                }
                PropertyDescriptor::Generic(data)
                    if data.enumerable != Some(false) && data.configurable != Some(false) =>
                {
                    return Ok(());
                }
                descriptor => {
                    let descriptor = preserve_dense_property_fields(descriptor, current_value);
                    self.define_missing_property_raw(receiver, key, descriptor)?;
                    let removed = self.delete_dense_array_value(receiver, key)?;
                    debug_assert!(removed, "published descriptor replaces one dense property");
                    return Ok(());
                }
            }
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let property = self.shapes.lookup(snapshot.shape, key);
        if let Some(property) = property
            && let Some(current) = self.stored_property_from_snapshot(snapshot, property)?
        {
            self.redefine_present_property(
                receiver, object, snapshot, key, property, current, descriptor,
            )?;
            return self.grow_array_length_for_index_property(receiver, key);
        }
        if property.is_none()
            && let Some(current_value) = self.function_metadata_property(receiver, key)?
        {
            return self.redefine_virtual_data_property(
                receiver,
                object,
                snapshot,
                key,
                current_value,
                descriptor,
            );
        }
        if !snapshot.extensible {
            return Err(ExecutionError::NonExtensibleObject(receiver));
        }
        self.define_missing_property(receiver, key, descriptor)
    }

    /// Implements Array exotic `[[DefineOwnProperty]]` for the non-configurable length slot.
    ///
    /// Shrinking is transactional in the ECMAScript sense: length is published first, indexed
    /// properties are deleted in descending order, and a failed delete restores length to the
    /// blocking index plus one. A requested `writable: false` is delayed until that transaction
    /// either succeeds or has restored its observable failure state.
    fn array_set_length_descriptor(
        &mut self,
        receiver: Value,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let Some(value) = (match descriptor {
            PropertyDescriptor::Data(data) => data.value,
            PropertyDescriptor::Generic(_) | PropertyDescriptor::Accessor(_) => None,
        }) else {
            return self.redefine_array_length(receiver, descriptor);
        };
        let new_length = self.primitive_array_length(value)?;
        self.array_set_length_descriptor_canonical(receiver, descriptor, new_length)
    }

    /// Applies ArraySetLength after the caller has completed both observable conversions.
    pub(crate) fn array_set_length_descriptor_canonical(
        &mut self,
        receiver: Value,
        descriptor: PropertyDescriptor,
        new_length: u32,
    ) -> Result<(), ExecutionError> {
        let canonical_value = safe_integer_value(u64::from(new_length));
        let length_key = PropertyKey::Atom(self.length_atom()?);
        let old_descriptor = self
            .complete_own_property_descriptor(receiver, length_key)?
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let PropertyDescriptor::Data(old_data) = old_descriptor else {
            return Err(ExecutionError::InvalidArrayLength);
        };
        let old_length = old_data
            .value
            .and_then(numeric_value)
            .ok_or(ExecutionError::InvalidArrayLength)? as u32;
        let PropertyDescriptor::Data(mut new_data) = descriptor else {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        };
        new_data.value = Some(canonical_value);
        if new_length >= old_length {
            return self.redefine_array_length(receiver, PropertyDescriptor::Data(new_data));
        }
        if old_data.writable != Some(true) {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }

        let delay_read_only = new_data.writable == Some(false);
        if delay_read_only {
            new_data.writable = Some(true);
        }
        self.redefine_array_length(receiver, PropertyDescriptor::Data(new_data))?;
        let indexed_keys = self.array_shrink_keys(receiver, new_length)?;
        for (index, key) in indexed_keys {
            if !self.delete_own_data_property(receiver, key)? {
                new_data.value = Some(safe_integer_value(u64::from(index) + 1));
                if delay_read_only {
                    new_data.writable = Some(false);
                }
                self.truncate_dense_array(receiver, index + 1)?;
                self.redefine_array_length(receiver, PropertyDescriptor::Data(new_data))?;
                return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
            }
        }
        self.truncate_dense_array(receiver, new_length)?;
        if delay_read_only {
            self.redefine_array_length(
                receiver,
                PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: None,
                    writable: Some(false),
                    enumerable: None,
                    configurable: None,
                }),
            )?;
        }
        Ok(())
    }

    /// Keeps the synchronous exotic boundary honest until observable object ToNumber can suspend.
    fn primitive_array_length(&mut self, value: Value) -> Result<u32, ExecutionError> {
        if self.is_object_value(value) {
            return Err(ExecutionError::UnsupportedNumberConversion(value));
        }
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        if !number.is_finite()
            || number < 0.0
            || number.fract() != 0.0
            || number > f64::from(u32::MAX)
        {
            return Err(ExecutionError::InvalidArrayLength);
        }
        Ok(number as u32)
    }

    /// Applies ordinary descriptor validation to the intrinsic Array length property.
    fn redefine_array_length(
        &mut self,
        receiver: Value,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let key = PropertyKey::Atom(self.length_atom()?);
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let property = self
            .shapes
            .lookup(snapshot.shape, key)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let current = self
            .stored_property_from_snapshot(snapshot, property)?
            .ok_or(ExecutionError::InvalidArrayLength)?;
        self.redefine_present_property(
            receiver, object, snapshot, key, property, current, descriptor,
        )
    }

    /// Snapshots every index removed by ArraySetLength in strict descending numeric order.
    fn array_shrink_keys(
        &mut self,
        receiver: Value,
        new_length: u32,
    ) -> Result<Vec<(u32, PropertyKey)>, ExecutionError> {
        let (_, snapshot) = self.object_snapshot(receiver)?;
        let keys = self.ordinary_own_property_keys(receiver, snapshot)?;
        let mut indexed = Vec::new();
        indexed
            .try_reserve_exact(keys.len())
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        for key in keys {
            let Some(index) = self.array_property_index(key) else {
                continue;
            };
            if index >= new_length {
                indexed.push((index, key));
            }
        }
        indexed.sort_unstable_by_key(|entry| core::cmp::Reverse(entry.0));
        Ok(indexed)
    }

    /// Rejects an Array index definition before publishing a property past read-only length.
    fn guard_array_index_growth(
        &mut self,
        receiver: Value,
        index: u32,
    ) -> Result<(), ExecutionError> {
        let length_key = PropertyKey::Atom(self.length_atom()?);
        let descriptor = self
            .complete_own_property_descriptor(receiver, length_key)?
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let PropertyDescriptor::Data(data) = descriptor else {
            return Err(ExecutionError::InvalidArrayLength);
        };
        let length = data
            .value
            .and_then(numeric_value)
            .ok_or(ExecutionError::InvalidArrayLength)? as u32;
        if index >= length && data.writable != Some(true) {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        Ok(())
    }

    /// Internal Array algorithms use the same exotic path as Object.defineProperty.
    pub(crate) fn set_array_length_value(
        &mut self,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.array_set_length_descriptor(
            receiver,
            PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(value),
                writable: None,
                enumerable: None,
                configurable: None,
            }),
        )
    }

    /// Creates a normalized payload, reusing a retained tombstone slot when one exists.
    fn define_missing_property(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        if let Some(index) = self.array_property_index(key)
            && index <= tuning::arrays::MAX_DENSE_ELEMENT_INDEX
            && let PropertyDescriptor::Data(data) = descriptor
            && data.writable == Some(true)
            && data.enumerable == Some(true)
            && data.configurable == Some(true)
            && let Some(value) = data.value
            && self.set_dense_array_value(receiver, index, value)?
        {
            return self.grow_array_length_for_index_property(receiver, key);
        }
        self.define_missing_property_raw(receiver, key, descriptor)?;
        self.grow_array_length_for_index_property(receiver, key)
    }

    /// Publishes a missing ordinary slot before applying Array index length growth.
    fn define_missing_property_raw(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        match descriptor {
            PropertyDescriptor::Generic(descriptor) => self.publish_missing_property(
                receiver,
                key,
                PropertyKind::Data,
                undefined,
                PropertyAttributes::data(
                    false,
                    descriptor.enumerable.unwrap_or(false),
                    descriptor.configurable.unwrap_or(false),
                ),
            ),
            PropertyDescriptor::Data(descriptor) => self.publish_missing_property(
                receiver,
                key,
                PropertyKind::Data,
                descriptor.value.unwrap_or(undefined),
                PropertyAttributes::data(
                    descriptor.writable.unwrap_or(false),
                    descriptor.enumerable.unwrap_or(false),
                    descriptor.configurable.unwrap_or(false),
                ),
            ),
            PropertyDescriptor::Accessor(descriptor) => {
                let (receiver, pair) = self.allocate_accessor_pair(
                    receiver,
                    key,
                    descriptor.getter.unwrap_or(undefined),
                    descriptor.setter.unwrap_or(undefined),
                )?;
                self.publish_missing_property(
                    receiver,
                    key,
                    PropertyKind::Accessor,
                    pair,
                    PropertyAttributes::accessor(
                        descriptor.enumerable.unwrap_or(false),
                        descriptor.configurable.unwrap_or(false),
                    ),
                )
            }
        }
    }

    /// Applies one validated descriptor to a present fixed logical slot.
    #[allow(clippy::too_many_arguments)]
    fn redefine_present_property(
        &mut self,
        receiver: Value,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: PropertyKey,
        property: PropertyLookup,
        current: StoredProperty,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        self.validate_property_redefinition(receiver, property, current, descriptor)?;
        let enumerable = descriptor
            .enumerable()
            .unwrap_or_else(|| property.attributes.enumerable());
        let configurable = descriptor
            .configurable()
            .unwrap_or_else(|| property.attributes.configurable());
        match descriptor {
            PropertyDescriptor::Generic(_) => {
                let mut attributes = match property.kind {
                    PropertyKind::Data => PropertyAttributes::data(
                        property.attributes.writable(),
                        enumerable,
                        configurable,
                    ),
                    PropertyKind::Accessor => {
                        PropertyAttributes::accessor(enumerable, configurable)
                    }
                };
                if property.attributes.virtual_origin() {
                    attributes = attributes.with_virtual_origin();
                }
                let shape = self
                    .shapes
                    .transition_reconfigure_kind(snapshot.shape, key, property.kind, attributes)
                    .map_err(ExecutionError::Shape)?;
                self.set_object_shape(object, shape)
            }
            PropertyDescriptor::Data(descriptor) => self.apply_data_descriptor(
                receiver,
                object,
                snapshot,
                key,
                property,
                current,
                descriptor,
                enumerable,
                configurable,
            ),
            PropertyDescriptor::Accessor(descriptor) => self.apply_accessor_descriptor(
                receiver,
                object,
                snapshot,
                key,
                current,
                descriptor,
                enumerable,
                configurable,
                property.attributes.virtual_origin(),
            ),
        }
    }

    /// Rejects all non-configurable common, kind, and kind-specific mutations.
    fn validate_property_redefinition(
        &mut self,
        receiver: Value,
        property: PropertyLookup,
        current: StoredProperty,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        if property.attributes.configurable() {
            return Ok(());
        }
        if descriptor.configurable() == Some(true)
            || descriptor
                .enumerable()
                .is_some_and(|value| value != property.attributes.enumerable())
            || descriptor
                .requested_kind()
                .is_some_and(|kind| kind != property.kind)
        {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        match (current, descriptor) {
            (StoredProperty::Data(current), PropertyDescriptor::Data(descriptor))
                if !property.attributes.writable() =>
            {
                if descriptor.writable == Some(true) {
                    return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                }
                if let Some(value) = descriptor.value
                    && !self.same_value(value, current)?
                {
                    return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                }
            }
            (StoredProperty::Accessor { pair, .. }, PropertyDescriptor::Accessor(descriptor)) => {
                if let Some(getter) = descriptor.getter
                    && !self.same_value(getter, pair.getter)?
                {
                    return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                }
                if let Some(setter) = descriptor.setter
                    && !self.same_value(setter, pair.setter)?
                {
                    return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Applies data flags/value, converting an accessor pair back to a direct Value when allowed.
    #[allow(clippy::too_many_arguments)]
    fn apply_data_descriptor(
        &mut self,
        _receiver: Value,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: PropertyKey,
        property: PropertyLookup,
        current: StoredProperty,
        descriptor: DataPropertyDescriptor,
        enumerable: bool,
        configurable: bool,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let (current_value, current_writable) = match current {
            StoredProperty::Data(value) => (value, property.attributes.writable()),
            StoredProperty::Accessor { .. } => (undefined, false),
        };
        let value = descriptor.value.unwrap_or(current_value);
        let mut attributes = PropertyAttributes::data(
            descriptor.writable.unwrap_or(current_writable),
            enumerable,
            configurable,
        );
        if property.attributes.virtual_origin() {
            attributes = attributes.with_virtual_origin();
        }
        let shape = self
            .shapes
            .transition_reconfigure_kind(snapshot.shape, key, PropertyKind::Data, attributes)
            .map_err(ExecutionError::Shape)?;
        if descriptor.value.is_some() || property.kind != PropertyKind::Data {
            self.update_property_slot(snapshot, key, property.slot, value)?;
        }
        self.set_object_shape(object, shape)
    }

    /// Applies accessor flags/pair fields, allocating a pair only for a kind conversion.
    #[allow(clippy::too_many_arguments)]
    fn apply_accessor_descriptor(
        &mut self,
        receiver: Value,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: PropertyKey,
        current: StoredProperty,
        descriptor: AccessorPropertyDescriptor,
        enumerable: bool,
        configurable: bool,
        virtual_origin: bool,
    ) -> Result<(), ExecutionError> {
        let mut attributes = PropertyAttributes::accessor(enumerable, configurable);
        if virtual_origin {
            attributes = attributes.with_virtual_origin();
        }
        if let StoredProperty::Accessor { reference, .. } = current {
            let shape = self
                .shapes
                .transition_reconfigure_kind(
                    snapshot.shape,
                    key,
                    PropertyKind::Accessor,
                    attributes,
                )
                .map_err(ExecutionError::Shape)?;
            self.update_accessor_pair(reference, descriptor.getter, descriptor.setter)?;
            return self.set_object_shape(object, shape);
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let (receiver, pair) = self.allocate_accessor_pair(
            receiver,
            key,
            descriptor.getter.unwrap_or(undefined),
            descriptor.setter.unwrap_or(undefined),
        )?;
        self.replace_property_slot(receiver, key, PropertyKind::Accessor, pair, attributes)
    }

    /// Materializes a configurable virtual function field into ordinary data/accessor storage.
    #[allow(clippy::too_many_arguments)]
    fn redefine_virtual_data_property(
        &mut self,
        receiver: Value,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: PropertyKey,
        current_value: Value,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let current_attributes = PropertyAttributes::data(false, false, true);
        let current = StoredProperty::Data(current_value);
        let lookup = PropertyLookup {
            slot: 0,
            kind: PropertyKind::Data,
            attributes: current_attributes,
        };
        self.validate_property_redefinition(receiver, lookup, current, descriptor)?;
        let enumerable = descriptor.enumerable().unwrap_or(false);
        let configurable = descriptor.configurable().unwrap_or(true);
        match descriptor {
            PropertyDescriptor::Generic(_) => self.add_property_slot_with_kind(
                object,
                snapshot,
                key,
                PropertyKind::Data,
                current_value,
                PropertyAttributes::data(false, enumerable, configurable).with_virtual_origin(),
            ),
            PropertyDescriptor::Data(descriptor) => self.add_property_slot_with_kind(
                object,
                snapshot,
                key,
                PropertyKind::Data,
                descriptor.value.unwrap_or(current_value),
                PropertyAttributes::data(
                    descriptor.writable.unwrap_or(false),
                    enumerable,
                    configurable,
                )
                .with_virtual_origin(),
            ),
            PropertyDescriptor::Accessor(descriptor) => {
                let undefined = Value::from_immediate(Immediate::Undefined);
                let (receiver, pair) = self.allocate_accessor_pair(
                    receiver,
                    key,
                    descriptor.getter.unwrap_or(undefined),
                    descriptor.setter.unwrap_or(undefined),
                )?;
                let (object, snapshot) = self.object_snapshot(receiver)?;
                self.add_property_slot_with_kind(
                    object,
                    snapshot,
                    key,
                    PropertyKind::Accessor,
                    pair,
                    PropertyAttributes::accessor(enumerable, configurable).with_virtual_origin(),
                )
            }
        }
    }

    /// Publishes a new property after removing any retained absence marker from chronology.
    fn publish_missing_property(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        kind: PropertyKind,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        let (object, snapshot) = self.object_snapshot(receiver)?;
        if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
            debug_assert!(
                self.raw_property_value_from_snapshot(snapshot, property)?
                    .is_none()
            );
            self.remove_property_slot(object, snapshot, key)?;
            let (object, snapshot) = self.object_snapshot(receiver)?;
            return self
                .add_property_slot_with_kind(object, snapshot, key, kind, value, attributes);
        }
        self.add_property_slot_with_kind(object, snapshot, key, kind, value, attributes)
    }

    /// Re-resolves an existing slot after pair allocation, then publishes payload before shape kind.
    fn replace_property_slot(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        kind: PropertyKind,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let property = self
            .shapes
            .lookup(snapshot.shape, key)
            .expect("kind conversion retains its logical property slot");
        let shape = self
            .shapes
            .transition_reconfigure_kind(snapshot.shape, key, kind, attributes)
            .map_err(ExecutionError::Shape)?;
        self.update_property_slot(snapshot, key, property.slot, value)?;
        self.set_object_shape(object, shape)
    }

    /// Populates a rooted fresh object in FromPropertyDescriptor field order for either kind.
    pub(crate) fn materialize_property_descriptor(
        &mut self,
        result: Value,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        match descriptor {
            PropertyDescriptor::Data(descriptor) => {
                if let Some(value) = descriptor.value {
                    let value_atom = self.intern_intrinsic_name(b"value")?;
                    self.set_own_data_property(result, value_atom, value)?;
                }
                if let Some(writable) = descriptor.writable {
                    let writable_atom = self.intern_intrinsic_name(b"writable")?;
                    self.set_own_data_property(
                        result,
                        writable_atom,
                        Value::from_immediate(if writable {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    )?;
                }
            }
            PropertyDescriptor::Accessor(descriptor) => {
                if let Some(getter) = descriptor.getter {
                    let get_atom = self.intern_intrinsic_name(b"get")?;
                    self.set_own_data_property(result, get_atom, getter)?;
                }
                if let Some(setter) = descriptor.setter {
                    let set_atom = self.intern_intrinsic_name(b"set")?;
                    self.set_own_data_property(result, set_atom, setter)?;
                }
            }
            PropertyDescriptor::Generic(_) => {}
        }
        if let Some(enumerable) = descriptor.enumerable() {
            let enumerable_atom = self.intern_intrinsic_name(b"enumerable")?;
            self.set_own_data_property(
                result,
                enumerable_atom,
                Value::from_immediate(if enumerable {
                    Immediate::True
                } else {
                    Immediate::False
                }),
            )?;
        }
        if let Some(configurable) = descriptor.configurable() {
            let configurable_atom = self.intern_intrinsic_name(b"configurable")?;
            self.set_own_data_property(
                result,
                configurable_atom,
                Value::from_immediate(if configurable {
                    Immediate::True
                } else {
                    Immediate::False
                }),
            )?;
        }
        Ok(())
    }

    /// Returns one complete stored descriptor for targeted invariant tests.
    pub(crate) fn complete_own_property_descriptor(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<Option<PropertyDescriptor>, ExecutionError> {
        let key = key.into();
        if let Some(value) = self.dense_array_value(receiver, key)? {
            return Ok(Some(PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            })));
        }
        if let Some(indexed) = self.typed_array_index_get(receiver, key)? {
            return Ok(indexed.map(|value| {
                PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: Some(value),
                    writable: Some(true),
                    enumerable: Some(true),
                    configurable: Some(true),
                })
            }));
        }
        if self.is_string_wrapper(receiver) {
            let length = self.length_atom()?;
            if key == PropertyKey::Atom(length) {
                return Ok(Some(PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: Some(safe_integer_value(
                        self.string_value_length(receiver)? as u64
                    )),
                    writable: Some(false),
                    enumerable: Some(false),
                    configurable: Some(false),
                })));
            }
            if let Some(atom) = key.atom()
                && let Some(index) = self
                    .atoms
                    .get(atom)
                    .and_then(|name| crate::property::keys::array_index(name.as_view()))
                && let Some(value) = self.string_index_value(receiver, index as usize)?
            {
                return Ok(Some(PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: Some(value),
                    writable: Some(false),
                    enumerable: Some(true),
                    configurable: Some(false),
                })));
            }
        }
        if self.is_strict_arguments_restricted_property(receiver, key)? {
            let undefined = Value::from_immediate(Immediate::Undefined);
            return Ok(Some(PropertyDescriptor::Accessor(
                AccessorPropertyDescriptor {
                    getter: Some(undefined),
                    setter: Some(undefined),
                    enumerable: Some(false),
                    configurable: Some(false),
                },
            )));
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        if matches!(object, ObjectReceiver::ModuleNamespace(_))
            && let Some(descriptor) = self.module_namespace_property_descriptor(receiver, key)?
        {
            return Ok(Some(descriptor));
        }
        let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
            if let Some(value) = self.function_metadata_property(receiver, key)? {
                return Ok(Some(PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: Some(value),
                    writable: Some(false),
                    enumerable: Some(false),
                    configurable: Some(true),
                })));
            }
            if self.is_function_prototype_property(receiver, key) {
                self.intrinsic_property_atoms.prototype = key.atom();
                let value = self.ensure_function_prototype(receiver)?;
                let read_only = self.has_read_only_prototype(receiver)?;
                return Ok(Some(PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: Some(value),
                    writable: Some(!read_only),
                    enumerable: Some(false),
                    configurable: Some(false),
                })));
            }
            return Ok(None);
        };
        let Some(stored) = self.stored_property_from_snapshot(snapshot, property)? else {
            return Ok(None);
        };
        Ok(Some(match stored {
            StoredProperty::Data(value) => PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(self.mapped_argument_value(receiver, key)?.unwrap_or(value)),
                writable: Some(property.attributes.writable()),
                enumerable: Some(property.attributes.enumerable()),
                configurable: Some(property.attributes.configurable()),
            }),
            StoredProperty::Accessor { pair, .. } => {
                PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                    getter: Some(pair.getter),
                    setter: Some(pair.setter),
                    enumerable: Some(property.attributes.enumerable()),
                    configurable: Some(property.attributes.configurable()),
                })
            }
        }))
    }
}

/// Completes a descriptor against an existing dense element whose attributes are all true.
#[inline(always)]
fn preserve_dense_property_fields(
    descriptor: PropertyDescriptor,
    current_value: Value,
) -> PropertyDescriptor {
    match descriptor {
        PropertyDescriptor::Generic(descriptor) => {
            PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(current_value),
                writable: Some(true),
                enumerable: Some(descriptor.enumerable.unwrap_or(true)),
                configurable: Some(descriptor.configurable.unwrap_or(true)),
            })
        }
        PropertyDescriptor::Data(descriptor) => PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(descriptor.value.unwrap_or(current_value)),
            writable: Some(descriptor.writable.unwrap_or(true)),
            enumerable: Some(descriptor.enumerable.unwrap_or(true)),
            configurable: Some(descriptor.configurable.unwrap_or(true)),
        }),
        PropertyDescriptor::Accessor(descriptor) => {
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: descriptor.getter,
                setter: descriptor.setter,
                enumerable: Some(descriptor.enumerable.unwrap_or(true)),
                configurable: Some(descriptor.configurable.unwrap_or(true)),
            })
        }
    }
}
