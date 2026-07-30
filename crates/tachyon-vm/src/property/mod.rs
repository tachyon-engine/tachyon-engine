//! Ordinary property lookup, data descriptors, and intrinsic property atoms.

mod accessor;
pub(crate) mod copy;
mod descriptor;
mod descriptor_parse;
mod function;
mod keys;
mod private_field;
mod storage;

use super::*;
pub(crate) use accessor::{
    PropertyRead, PropertyReadResolution, PropertyWrite, PropertyWriteResolution,
    TypedArrayIndexSetMode,
};
pub(crate) use descriptor_parse::{
    ArrayLengthSetConsumer, PendingDefineProperties, PendingPropertyDescriptor,
};
pub(crate) use keys::array_index;

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
        if let Some(indexed) = self.typed_array_index_get(receiver, key)? {
            return Ok(indexed);
        }
        let mut current = if self.is_string_value(receiver) || self.is_string_wrapper(receiver) {
            let length = self.length_atom()?;
            if key == PropertyKey::Atom(length) {
                let length = self.string_value_length(receiver)?;
                let length =
                    i32::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
                return Ok(Some(Value::from_i32(length)));
            }
            self.realm
                .string_prototype
                .expect("String prototype initializes before primitive String access")
        } else if numeric_value(receiver).is_some() {
            self.realm
                .number_prototype
                .expect("Number prototype initializes before property access")
        } else if self.is_bigint_value(receiver) {
            self.realm
                .bigint_prototype
                .expect("BigInt prototype initializes before property access")
        } else if matches!(
            receiver.as_immediate(),
            Some(Immediate::True | Immediate::False)
        ) {
            self.realm
                .boolean_prototype
                .expect("Boolean prototype initializes before property access")
        } else if self.is_symbol_value(receiver) {
            self.realm
                .symbol_prototype
                .expect("Symbol prototype initializes before property access")
        } else {
            receiver
        };
        loop {
            if let Some(value) = self.dense_array_value(current, key)? {
                return Ok(Some(value));
            }
            let (object, snapshot) = self.object_snapshot(current)?;
            if matches!(object, ObjectReceiver::ModuleNamespace(_))
                && let Some(value) = self.module_namespace_property(current, key)?
            {
                return Ok(Some(value));
            }
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
    pub(crate) fn has_own_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<bool, ExecutionError> {
        Ok(self
            .complete_own_property_descriptor(receiver, key)?
            .is_some())
    }

    /// Resolves virtual function fields and ordinary own slots with their exact data flags.
    pub(crate) fn own_data_property_with_attributes(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<Option<(Value, PropertyAttributes)>, ExecutionError> {
        let key = key.into();
        if let Some(indexed) = self.typed_array_index_get(receiver, key)? {
            return Ok(indexed.map(|value| (value, PropertyAttributes::data(true, true, true))));
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        if matches!(object, ObjectReceiver::ModuleNamespace(_))
            && let Some(descriptor) = self.module_namespace_property_descriptor(receiver, key)?
            && let PropertyDescriptor::Data(descriptor) = descriptor
        {
            return Ok(Some((
                descriptor
                    .value
                    .expect("complete namespace descriptor has a value"),
                PropertyAttributes::data(true, true, false),
            )));
        }
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
            let read_only = self.has_read_only_prototype(receiver)?;
            return Ok(Some((
                value,
                PropertyAttributes::data(!read_only, false, false),
            )));
        }
        Ok(None)
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

    pub(crate) fn done_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.done {
            return Ok(atom);
        }
        let atom = self.intern_intrinsic_name(b"done")?;
        self.intrinsic_property_atoms.done = Some(atom);
        Ok(atom)
    }

    pub(crate) fn value_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.value {
            return Ok(atom);
        }
        let atom = self.intern_intrinsic_name(b"value")?;
        self.intrinsic_property_atoms.value = Some(atom);
        Ok(atom)
    }
}
