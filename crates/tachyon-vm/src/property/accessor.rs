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
pub(crate) enum PropertyReadResolution {
    Read(PropertyRead),
    Proxy(Value),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PropertyWrite {
    Complete(bool),
    Setter(Value),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PropertyWriteResolution {
    Write(PropertyWrite),
    Proxy(Value),
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
    /// Resolves a live mapped-arguments index to its owning activation without changing Frame size.
    fn mapped_argument_frame(
        &mut self,
        target: Value,
        key: PropertyKey,
    ) -> Result<Option<(Frame, u32)>, ExecutionError> {
        let Some(raw) = target.as_heap_ref() else {
            return Ok(None);
        };
        let Ok(arguments) = self
            .heap
            .checked_reference(raw, self.types.arguments_object)
        else {
            return Ok(None);
        };
        let (depth, base, count, code, function) = self.heap.with_running_scope(|scope| {
            let arguments = scope.root(arguments).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(arguments, self.types.arguments_object)
                    .map(|object| {
                        (
                            object.mapped_frame_depth,
                            object.mapped_base,
                            object.mapped_parameter_count,
                            object.mapped_code,
                            object.mapped_function,
                        )
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if depth == u32::MAX {
            return Ok(None);
        }
        let (Some(code), Some(function)) = (code, function) else {
            return Ok(None);
        };
        let Some(index) = key.atom().and_then(|atom| {
            self.atoms
                .get(atom)
                .and_then(|name| crate::property::keys::array_index(name.as_view()))
        }) else {
            return Ok(None);
        };
        if index >= count {
            return Ok(None);
        }
        let Some(frame) = self.fiber.frames.get(depth as usize).copied() else {
            return Ok(None);
        };
        if frame.code != code || frame.function != function || frame.base != base {
            return Ok(None);
        }
        Ok(Some((frame, index)))
    }

    /// Reads a mapped parameter while its owner activation is still present on the fiber.
    fn mapped_argument_value(
        &mut self,
        target: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let Some((frame, index)) = self.mapped_argument_frame(target, key)? else {
            return Ok(None);
        };
        let (_, snapshot) = self.object_snapshot(target)?;
        let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
            return Ok(None);
        };
        if !property.attributes.writable()
            || !matches!(
                self.stored_property_from_snapshot(snapshot, property)?,
                Some(StoredProperty::Data(_))
            )
        {
            return Ok(None);
        }
        self.read(frame.base, index).map(Some)
    }

    /// Publishes an arguments-index write back to the owning simple parameter register.
    pub(crate) fn sync_mapped_argument(
        &mut self,
        target: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if let Some((frame, index)) = self.mapped_argument_frame(target, key)? {
            self.write(frame.base, index, value)?;
        }
        Ok(())
    }

    /// Materializes one String-exotic UTF-16 code-unit value for descriptor consumers.
    pub(crate) fn string_index_value(
        &mut self,
        receiver: Value,
        index: usize,
    ) -> Result<Option<Value>, ExecutionError> {
        if index >= self.string_value_length(receiver)? {
            return Ok(None);
        }
        let string_receiver = self.string_primitive_value(receiver)?;
        let raw = string_receiver
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(string_receiver))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(ExecutionError::HeapReference)?;
        let unit = self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(|string| string.code_unit_at(index).expect("checked index"))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let value = self.allocate_runtime_string(
            JsString::try_from_utf16(&[unit]).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        Ok(Some(value))
    }

    /// Materializes one String iterator code point and returns its next UTF-16 index.
    pub(crate) fn string_code_point_value_at(
        &mut self,
        receiver: Value,
        index: usize,
    ) -> Result<Option<(Value, usize)>, ExecutionError> {
        let length = self.string_value_length(receiver)?;
        if index >= length {
            return Ok(None);
        }
        let string_receiver = self.string_primitive_value(receiver)?;
        let raw = string_receiver
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(string_receiver))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(ExecutionError::HeapReference)?;
        let units = self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(|string| {
                        let first = string.code_unit_at(index).expect("checked index");
                        let second = string.code_unit_at(index + 1);
                        if (0xd800..=0xdbff).contains(&first)
                            && second.is_some_and(|unit| (0xdc00..=0xdfff).contains(&unit))
                        {
                            vec![first, second.expect("checked surrogate pair")]
                        } else {
                            vec![first]
                        }
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let next = index + units.len();
        let value = self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )?;
        Ok(Some((value, next)))
    }

    /// Resolves an ordinary read while retaining the original receiver for accessor `this`.
    pub(crate) fn resolve_property_read(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<PropertyRead, ExecutionError> {
        self.resolve_property_read_from(receiver, key)
    }

    /// Resolves a property from one target; callers retain the accessor receiver independently.
    pub(crate) fn resolve_property_read_from(
        &mut self,
        target: Value,
        key: PropertyKey,
    ) -> Result<PropertyRead, ExecutionError> {
        match self.resolve_property_read_until_proxy(target, key)? {
            PropertyReadResolution::Read(read) => Ok(read),
            PropertyReadResolution::Proxy(proxy) => Err(ExecutionError::NotObject(proxy)),
        }
    }

    /// Performs the ordinary read loop once and returns when an exotic Proxy owns the remainder.
    pub(crate) fn resolve_property_read_until_proxy(
        &mut self,
        target: Value,
        key: PropertyKey,
    ) -> Result<PropertyReadResolution, ExecutionError> {
        if let Some(value) = self.mapped_argument_value(target, key)? {
            return Ok(PropertyReadResolution::Read(PropertyRead::Data(value)));
        }
        if let Some(raw) = target.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.regexp_object)
                .is_ok()
        {
            let source = self.intern_intrinsic_name(b"source")?;
            let flags = self.intern_intrinsic_name(b"flags")?;
            let (regexp_source, regexp_flags) = self.regexp_data(target)?;
            if key == PropertyKey::Atom(source) {
                return Ok(PropertyReadResolution::Read(PropertyRead::Data(
                    regexp_source,
                )));
            }
            if key == PropertyKey::Atom(flags) {
                return Ok(PropertyReadResolution::Read(PropertyRead::Data(
                    regexp_flags,
                )));
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
                    return self.regexp_flag_enabled(target, flag).map(|enabled| {
                        PropertyReadResolution::Read(PropertyRead::Data(Value::from_immediate(
                            if enabled {
                                Immediate::True
                            } else {
                                Immediate::False
                            },
                        )))
                    });
                }
            }
        }
        let mut current = if self.is_string_value(target) || self.is_string_wrapper(target) {
            let length = self.length_atom()?;
            if key == PropertyKey::Atom(length) {
                return self.string_value_length(target).and_then(|length| {
                    i32::try_from(length)
                        .map(Value::from_i32)
                        .map(|value| PropertyReadResolution::Read(PropertyRead::Data(value)))
                        .map_err(|_| ExecutionError::ArrayLengthOverflow)
                });
            }
            if let Some(atom) = key.atom()
                && let Some(index) = self
                    .atoms
                    .get(atom)
                    .and_then(|name| crate::property::keys::array_index(name.as_view()))
                && (index as usize) < self.string_value_length(target)?
            {
                let string_receiver = self.string_primitive_value(target)?;
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
                return Ok(PropertyReadResolution::Read(PropertyRead::Data(value)));
            }
            if self.is_string_wrapper(target) {
                target
            } else {
                self.realm
                    .string_prototype
                    .expect("String prototype initializes before primitive String access")
            }
        } else if numeric_value(target).is_some() {
            self.realm
                .number_prototype
                .expect("Number prototype initializes before property access")
        } else if matches!(
            target.as_immediate(),
            Some(Immediate::True | Immediate::False)
        ) {
            self.realm
                .boolean_prototype
                .expect("Boolean prototype initializes before property access")
        } else if self.is_symbol_value(target) {
            self.realm
                .symbol_prototype
                .expect("Symbol prototype initializes before property access")
        } else {
            target
        };
        loop {
            if self.is_proxy_value(current) {
                return Ok(PropertyReadResolution::Proxy(current));
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
                match self.stored_property_from_snapshot(snapshot, property)? {
                    Some(StoredProperty::Data(value)) => {
                        return Ok(PropertyReadResolution::Read(PropertyRead::Data(value)));
                    }
                    Some(StoredProperty::Accessor { pair, .. }) => {
                        return Ok(PropertyReadResolution::Read(PropertyRead::Accessor(
                            pair.getter,
                        )));
                    }
                    None => {}
                }
            } else {
                if let Some(value) = self.function_metadata_property(current, key)? {
                    return Ok(PropertyReadResolution::Read(PropertyRead::Data(value)));
                }
                if self.is_function_prototype_property(current, key) {
                    self.intrinsic_property_atoms.prototype = key.atom();
                    return self
                        .ensure_function_prototype(current)
                        .map(|value| PropertyReadResolution::Read(PropertyRead::Data(value)));
                }
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(PropertyReadResolution::Read(PropertyRead::Missing));
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
        match self.resolve_property_write_until_proxy(receiver, key, value)? {
            PropertyWriteResolution::Write(write) => Ok(write),
            PropertyWriteResolution::Proxy(proxy) => Err(ExecutionError::NotObject(proxy)),
        }
    }

    /// Runs ordinary assignment until one Proxy owns the remaining prototype-chain operation.
    pub(crate) fn resolve_property_write_until_proxy(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<PropertyWriteResolution, ExecutionError> {
        if self.is_string_wrapper(receiver) {
            let length = self.length_atom()?;
            let virtual_index = key.atom().is_some_and(|atom| {
                self.atoms
                    .get(atom)
                    .and_then(|name| crate::property::keys::array_index(name.as_view()))
                    .is_some_and(|index| {
                        self.string_value_length(receiver)
                            .is_ok_and(|length| (index as usize) < length)
                    })
            });
            if key == PropertyKey::Atom(length) || virtual_index {
                return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                    false,
                )));
            }
        }
        if let Some(raw) = receiver.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.regexp_object)
                .is_ok()
        {
            for name in [
                b"source".as_slice(),
                b"flags".as_slice(),
                b"hasIndices".as_slice(),
                b"global".as_slice(),
                b"ignoreCase".as_slice(),
                b"multiline".as_slice(),
                b"dotAll".as_slice(),
                b"unicode".as_slice(),
                b"unicodeSets".as_slice(),
                b"sticky".as_slice(),
            ] {
                let atom = self.intern_intrinsic_name(name)?;
                if key == PropertyKey::Atom(atom) {
                    return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                        false,
                    )));
                }
            }
        }
        let mut current = if self.is_string_value(receiver) {
            return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                false,
            )));
        } else if numeric_value(receiver).is_some() {
            self.realm
                .number_prototype
                .expect("Number prototype initializes before property access")
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
            let snapshot = match self.object_snapshot(current) {
                Ok((_, snapshot)) => snapshot,
                Err(_error) if self.is_proxy_value(current) => {
                    return Ok(PropertyWriteResolution::Proxy(current));
                }
                Err(error) => return Err(error),
            };
            if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
                match self.stored_property_from_snapshot(snapshot, property)? {
                    Some(StoredProperty::Data(_)) => {
                        if !property.attributes.writable() {
                            return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                                false,
                            )));
                        }
                        return self
                            .write_data_property_boolean(receiver, key, value)
                            .map(PropertyWriteResolution::Write);
                    }
                    Some(StoredProperty::Accessor { pair, .. }) => {
                        return Ok(PropertyWriteResolution::Write(
                            if pair.setter.as_immediate() == Some(Immediate::Undefined) {
                                PropertyWrite::Complete(false)
                            } else {
                                PropertyWrite::Setter(pair.setter)
                            },
                        ));
                    }
                    None if current == receiver => {
                        return self
                            .write_data_property_boolean(receiver, key, value)
                            .map(PropertyWriteResolution::Write);
                    }
                    None => {}
                }
            } else if self.is_function_metadata_property(current, key)? {
                return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                    false,
                )));
            } else if self.is_function_prototype_property(current, key) {
                if self.has_read_only_prototype(current)? {
                    return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                        false,
                    )));
                }
                return self
                    .write_data_property_boolean(receiver, key, value)
                    .map(PropertyWriteResolution::Write);
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return self
                    .write_data_property_boolean(receiver, key, value)
                    .map(PropertyWriteResolution::Write);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Runs OrdinarySet with a distinct target and receiver for Reflect.set's observable path.
    pub(crate) fn resolve_reflect_property_write(
        &mut self,
        target: Value,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<PropertyWrite, ExecutionError> {
        match self.resolve_reflect_property_write_until_proxy(target, receiver, key, value)? {
            PropertyWriteResolution::Write(write) => Ok(write),
            PropertyWriteResolution::Proxy(proxy) => Err(ExecutionError::NotObject(proxy)),
        }
    }

    /// Runs Reflect/OrdinarySet until a Proxy receiver or prototype owns the next operation.
    pub(crate) fn resolve_reflect_property_write_until_proxy(
        &mut self,
        target: Value,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<PropertyWriteResolution, ExecutionError> {
        if let Some(raw) = target.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.regexp_object)
                .is_ok()
        {
            for name in [
                b"source".as_slice(),
                b"flags".as_slice(),
                b"hasIndices".as_slice(),
                b"global".as_slice(),
                b"ignoreCase".as_slice(),
                b"multiline".as_slice(),
                b"dotAll".as_slice(),
                b"unicode".as_slice(),
                b"unicodeSets".as_slice(),
                b"sticky".as_slice(),
            ] {
                let atom = self.intern_intrinsic_name(name)?;
                if key == PropertyKey::Atom(atom) {
                    return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                        false,
                    )));
                }
            }
        }
        let mut current = target;
        loop {
            let descriptor = match self.complete_own_property_descriptor(current, key) {
                Ok(descriptor) => descriptor,
                Err(_error) if self.is_proxy_value(current) => {
                    return Ok(PropertyWriteResolution::Proxy(current));
                }
                Err(error) => return Err(error),
            };
            match descriptor {
                Some(PropertyDescriptor::Data(descriptor)) => {
                    if !descriptor.writable.unwrap_or(false) {
                        return Ok(PropertyWriteResolution::Write(PropertyWrite::Complete(
                            false,
                        )));
                    }
                    return self
                        .write_reflect_receiver(receiver, key, value)
                        .map(PropertyWriteResolution::Write);
                }
                Some(PropertyDescriptor::Accessor(descriptor)) => {
                    return Ok(PropertyWriteResolution::Write(match descriptor.setter {
                        Some(setter) if setter.as_immediate() != Some(Immediate::Undefined) => {
                            PropertyWrite::Setter(setter)
                        }
                        _ => PropertyWrite::Complete(false),
                    }));
                }
                Some(PropertyDescriptor::Generic(_)) => {
                    return Err(ExecutionError::InvalidPropertyDescriptor(current));
                }
                None => {
                    let prototype = self.object_snapshot(current)?.1.prototype;
                    if prototype.as_immediate() == Some(Immediate::Null) {
                        return self
                            .write_reflect_receiver(receiver, key, value)
                            .map(PropertyWriteResolution::Write);
                    }
                    if !self.is_object_value(prototype) {
                        return Err(ExecutionError::NotObject(prototype));
                    }
                    current = prototype;
                }
            }
        }
    }

    /// Applies OrdinarySet's receiver-own descriptor rules without invoking a setter twice.
    fn write_reflect_receiver(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<PropertyWrite, ExecutionError> {
        if !self.is_object_value(receiver) {
            return Ok(PropertyWrite::Complete(false));
        }
        match self.complete_own_property_descriptor(receiver, key)? {
            Some(PropertyDescriptor::Data(descriptor)) if !descriptor.writable.unwrap_or(false) => {
                Ok(PropertyWrite::Complete(false))
            }
            // OrdinarySet reaches this helper only from a writable data descriptor. A receiver
            // accessor blocks the data write; its setter is not invoked on this branch.
            Some(PropertyDescriptor::Accessor(_)) => Ok(PropertyWrite::Complete(false)),
            Some(PropertyDescriptor::Generic(_)) => {
                Err(ExecutionError::InvalidPropertyDescriptor(receiver))
            }
            Some(PropertyDescriptor::Data(_)) | None => match self
                .set_own_data_property(receiver, key, value)
            {
                Ok(()) => Ok(PropertyWrite::Complete(true)),
                Err(
                    ExecutionError::NonExtensibleObject(_) | ExecutionError::ReadOnlyProperty(_),
                ) => Ok(PropertyWrite::Complete(false)),
                Err(error) => Err(error),
            },
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
        let symbol_key = key.symbol().map(SymbolId::value);
        self.allocate_unpublished_accessor_pair(receiver, symbol_key, getter, setter)
    }

    /// Allocates one class-private accessor pair shared by every stamped instance.
    pub(crate) fn allocate_private_accessor_pair(
        &mut self,
        getter: Value,
        setter: Value,
    ) -> Result<Value, ExecutionError> {
        self.allocate_unpublished_accessor_pair(
            Value::from_immediate(Immediate::Undefined),
            None,
            getter,
            setter,
        )
        .map(|(_, pair)| pair)
    }

    /// Publishes one pair while independently rooting optional receiver and Symbol edges.
    fn allocate_unpublished_accessor_pair(
        &mut self,
        receiver: Value,
        symbol_key: Option<Value>,
        getter: Value,
        setter: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        self.validate_accessor_callable(getter)?;
        self.validate_accessor_callable(setter)?;
        let mut roots = AccessorAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            receiver,
            symbol_key,
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
