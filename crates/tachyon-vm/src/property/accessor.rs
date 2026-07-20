//! Accessor-pair allocation, checked slot recovery, and precise write barriers.

use super::super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum StoredProperty {
    Data(Value),
    Accessor {
        reference: GcRef<AccessorPair>,
        pair: AccessorPair,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PropertyRead {
    Missing,
    Data(Value),
    Accessor(Value),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PropertyWrite {
    Complete(bool),
    Setter(Value),
}

struct AccessorAllocationRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
    symbol_key: Option<Value>,
    getter: Value,
    setter: Value,
}

impl Trace for AccessorAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
        self.symbol_key.trace(tracer);
        self.getter.trace(tracer);
        self.setter.trace(tracer);
    }
}

impl Isolate {
    /// Resolves an ordinary read while retaining the original receiver for accessor `this`.
    pub(crate) fn resolve_property_read(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<PropertyRead, ExecutionError> {
        if let Some(raw) = receiver.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.regexp_object)
                .is_ok()
        {
            let source = self.intern_intrinsic_name(b"source")?;
            let flags = self.intern_intrinsic_name(b"flags")?;
            let (regexp_source, regexp_flags) = self.regexp_data(receiver)?;
            if key == PropertyKey::Atom(source) {
                return Ok(PropertyRead::Data(regexp_source));
            }
            if key == PropertyKey::Atom(flags) {
                return Ok(PropertyRead::Data(regexp_flags));
            }
            for (name, flag) in [
                (b"hasIndices".as_slice(), 100_u16),
                (b"global".as_slice(), 103_u16),
                (b"ignoreCase".as_slice(), 105_u16),
                (b"multiline".as_slice(), 109_u16),
                (b"dotAll".as_slice(), 115_u16),
                (b"unicode".as_slice(), 117_u16),
                (b"unicodeSets".as_slice(), 118_u16),
                (b"sticky".as_slice(), 121_u16),
            ] {
                let atom = self.intern_intrinsic_name(name)?;
                if key == PropertyKey::Atom(atom) {
                    return self.regexp_flag_enabled(receiver, flag).map(|enabled| {
                        PropertyRead::Data(Value::from_immediate(if enabled {
                            Immediate::True
                        } else {
                            Immediate::False
                        }))
                    });
                }
            }
        }
        let mut current = if self.is_string_value(receiver) || self.is_string_wrapper(receiver) {
            let length = self.length_atom()?;
            if key == PropertyKey::Atom(length) {
                return self.string_value_length(receiver).and_then(|length| {
                    i32::try_from(length)
                        .map(Value::from_i32)
                        .map(PropertyRead::Data)
                        .map_err(|_| ExecutionError::ArrayLengthOverflow)
                });
            }
            if let Some(atom) = key.atom()
                && let Some(index) = self
                    .atoms
                    .get(atom)
                    .and_then(|name| crate::property::keys::array_index(name.as_view()))
                && (index as usize) < self.string_value_length(receiver)?
            {
                let string_receiver = self.string_primitive_value(receiver)?;
                let raw = string_receiver
                    .as_heap_ref()
                    .expect("primitive String identity has a managed reference");
                let string = self
                    .heap
                    .checked_reference(raw, self.types.string)
                    .map_err(ExecutionError::HeapReference)?;
                let unit = self.heap.with_running_scope(|scope| {
                    let string = scope.root(string).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow(string, self.types.string)
                            .map(|string| {
                                string.code_unit_at(index as usize).expect("checked index")
                            })
                            .map_err(ExecutionError::NoGcBorrow)
                    })
                })?;
                let value = self.allocate_runtime_string(
                    JsString::try_from_utf16(&[unit]).map_err(ExecutionError::PropertyKeyString)?,
                )?;
                return Ok(PropertyRead::Data(value));
            }
            self.realm
                .string_prototype
                .expect("String prototype initializes before primitive String access")
        } else if numeric_value(receiver).is_some() {
            self.realm
                .number_prototype
                .expect("Number prototype initializes before property access")
        } else if self.is_symbol_value(receiver) {
            self.realm
                .symbol_prototype
                .expect("Symbol prototype initializes before property access")
        } else {
            receiver
        };
        loop {
            let (_, snapshot) = self.object_snapshot(current)?;
            if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
                match self.stored_property_from_snapshot(snapshot, property)? {
                    Some(StoredProperty::Data(value)) => return Ok(PropertyRead::Data(value)),
                    Some(StoredProperty::Accessor { pair, .. }) => {
                        return Ok(PropertyRead::Accessor(pair.getter));
                    }
                    None => {}
                }
            } else {
                if let Some(value) = self.function_metadata_property(current, key)? {
                    return Ok(PropertyRead::Data(value));
                }
                if self.is_function_prototype_property(current, key) {
                    self.intrinsic_property_atoms.prototype = key.atom();
                    return self
                        .ensure_function_prototype(current)
                        .map(PropertyRead::Data);
                }
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(PropertyRead::Missing);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Resolves an ordinary assignment to either a completed boolean result or one setter call.
    pub(crate) fn resolve_property_write(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<PropertyWrite, ExecutionError> {
        if let Some(raw) = receiver.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.regexp_object)
                .is_ok()
        {
            let source = self.intern_intrinsic_name(b"source")?;
            let flags = self.intern_intrinsic_name(b"flags")?;
            if key == PropertyKey::Atom(source) || key == PropertyKey::Atom(flags) {
                return Ok(PropertyWrite::Complete(false));
            }
        }
        let mut current = if self.is_string_value(receiver) {
            return Ok(PropertyWrite::Complete(false));
        } else if numeric_value(receiver).is_some() {
            self.realm
                .number_prototype
                .expect("Number prototype initializes before property access")
        } else if self.is_symbol_value(receiver) {
            self.realm
                .symbol_prototype
                .expect("Symbol prototype initializes before property access")
        } else {
            receiver
        };
        loop {
            let (_, snapshot) = self.object_snapshot(current)?;
            if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
                match self.stored_property_from_snapshot(snapshot, property)? {
                    Some(StoredProperty::Data(_)) => {
                        if !property.attributes.writable() {
                            return Ok(PropertyWrite::Complete(false));
                        }
                        return self.write_data_property_boolean(receiver, key, value);
                    }
                    Some(StoredProperty::Accessor { pair, .. }) => {
                        return Ok(
                            if pair.setter.as_immediate() == Some(Immediate::Undefined) {
                                PropertyWrite::Complete(false)
                            } else {
                                PropertyWrite::Setter(pair.setter)
                            },
                        );
                    }
                    None if current == receiver => {
                        return self.write_data_property_boolean(receiver, key, value);
                    }
                    None => {}
                }
            } else if self.is_function_metadata_property(current, key)? {
                return Ok(PropertyWrite::Complete(false));
            } else if self.is_function_prototype_property(current, key) {
                return self.write_data_property_boolean(receiver, key, value);
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return self.write_data_property_boolean(receiver, key, value);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Converts ordinary assignment rejection into the boolean consumed at the bytecode boundary.
    fn write_data_property_boolean(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<PropertyWrite, ExecutionError> {
        match self.set_own_data_property(receiver, key, value) {
            Ok(()) => Ok(PropertyWrite::Complete(true)),
            Err(ExecutionError::NonExtensibleObject(_) | ExecutionError::ReadOnlyProperty(_)) => {
                Ok(PropertyWrite::Complete(false))
            }
            Err(ExecutionError::NotObject(_)) if numeric_value(receiver).is_some() => {
                Ok(PropertyWrite::Complete(false))
            }
            Err(error) => Err(error),
        }
    }

    /// Recovers a present data value or validates and copies one accessor-pair payload.
    pub(super) fn stored_property_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        property: PropertyLookup,
    ) -> Result<Option<StoredProperty>, ExecutionError> {
        let Some(value) = self.raw_property_value_from_snapshot(snapshot, property)? else {
            return Ok(None);
        };
        if property.kind == PropertyKind::Data {
            return Ok(Some(StoredProperty::Data(value)));
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedAccessorDescriptor)?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.accessor_pair)
            .map_err(ExecutionError::HeapReference)?;
        let pair = self.heap.with_running_scope(|scope| {
            let local = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(local, self.types.accessor_pair)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok(Some(StoredProperty::Accessor { reference, pair }))
    }

    /// Allocates a normalized pair while rooting every unpublished accessor edge.
    pub(super) fn allocate_accessor_pair(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        getter: Value,
        setter: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        self.validate_accessor_callable(getter)?;
        self.validate_accessor_callable(setter)?;
        let mut roots = AccessorAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            receiver,
            symbol_key: key.symbol().map(SymbolId::value),
            getter,
            setter,
        };
        let pair = self
            .heap
            .try_allocate_with_gc(
                self.types.accessor_pair,
                0,
                0,
                AccessorPair {
                    getter: roots.getter,
                    setter: roots.setter,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok((roots.receiver, Value::from_heap_ref(pair.raw())))
    }

    /// Applies a partial accessor update and remembers each new young callable from the pair owner.
    pub(super) fn update_accessor_pair(
        &mut self,
        reference: GcRef<AccessorPair>,
        getter: Option<Value>,
        setter: Option<Value>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pair = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pair = no_gc
                    .borrow_mut(pair, self.types.accessor_pair)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if let Some(getter) = getter {
                    pair.getter = getter;
                }
                if let Some(setter) = setter {
                    pair.setter = setter;
                }
                Ok::<(), ExecutionError>(())
            })?;
            if let Some(getter) = getter {
                scope
                    .write_value_barrier(pair, getter)
                    .map_err(ExecutionError::HeapReference)?;
            }
            if let Some(setter) = setter {
                scope
                    .write_value_barrier(pair, setter)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Accepts only callable objects or the normalized ECMAScript undefined sentinel.
    pub(super) fn validate_accessor_callable(
        &mut self,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if value.as_immediate() == Some(Immediate::Undefined) {
            return Ok(());
        }
        self.resolve_function_object(value).map(|_| ())
    }
}
