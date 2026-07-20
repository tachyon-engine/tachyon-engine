//! Ordinary object slots, shapes, storage publication, and write barriers.

use super::{super::*, accessor::StoredProperty};

impl Isolate {
    /// Checks one resolved slot's tombstone state without interpreting its data/accessor payload.
    pub(crate) fn property_is_present_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        property: PropertyLookup,
    ) -> Result<bool, ExecutionError> {
        self.raw_property_value_from_snapshot(snapshot, property)
            .map(|value| value.is_some())
    }

    /// Reads a known ordinary snapshot's fixed slot without repeating receiver classification.
    pub(crate) fn data_property_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        key: impl Into<PropertyKey>,
    ) -> Result<Option<Value>, ExecutionError> {
        let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
            return Ok(None);
        };
        self.property_value_from_snapshot(snapshot, property)
    }

    /// Reads one resolved fixed slot and maps the retained deletion sentinel back to absence.
    pub(crate) fn property_value_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        property: PropertyLookup,
    ) -> Result<Option<Value>, ExecutionError> {
        match self.stored_property_from_snapshot(snapshot, property)? {
            Some(StoredProperty::Data(value)) => Ok(Some(value)),
            Some(StoredProperty::Accessor { .. }) => {
                Err(ExecutionError::UnsupportedAccessorDescriptor)
            }
            None => Ok(None),
        }
    }

    /// Copies one raw fixed slot and maps its retained deletion sentinel back to absence.
    pub(super) fn raw_property_value_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        property: PropertyLookup,
    ) -> Result<Option<Value>, ExecutionError> {
        let storage = snapshot
            .storage
            .expect("a non-empty shape always owns property storage");
        self.heap.with_running_scope(|scope| {
            let local = scope.root(storage).map_err(ExecutionError::Root)?;
            scope
                .with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.property_storage)
                        .map_err(ExecutionError::NoGcBorrow)
                        .map(|storage| storage.slots.get(property.slot as usize).copied())
                })
                .map(|value| value.filter(|value| value.as_immediate() != Some(Immediate::Hole)))
        })
    }

    /// Updates an existing slot in place or publishes an exactly sized replacement backing.
    pub(crate) fn set_own_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let key = key.into();
        if self.is_function_prototype_property(receiver, key) {
            self.intrinsic_property_atoms.prototype = key.atom();
            return self.set_function_prototype(receiver, value);
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
            match self.stored_property_from_snapshot(snapshot, property)? {
                Some(StoredProperty::Data(_)) => {
                    if !property.attributes.writable() {
                        return Err(ExecutionError::ReadOnlyProperty(receiver));
                    }
                    return self.update_property_slot(snapshot, key, property.slot, value);
                }
                Some(StoredProperty::Accessor { .. }) => {
                    return Err(ExecutionError::UnsupportedAccessorDescriptor);
                }
                None => {}
            }
            if !snapshot.extensible {
                return Err(ExecutionError::NonExtensibleObject(receiver));
            }
            let shape = self
                .shapes
                .transition_reconfigure_kind(
                    snapshot.shape,
                    key,
                    PropertyKind::Data,
                    PropertyAttributes::DEFAULT_DATA,
                )
                .map_err(ExecutionError::Shape)?;
            self.update_property_slot(snapshot, key, property.slot, value)?;
            return self.set_object_shape(object, shape);
        }
        if self.is_function_metadata_property(receiver, key)? {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        if !snapshot.extensible {
            return Err(ExecutionError::NonExtensibleObject(receiver));
        }
        self.add_property_slot(
            object,
            snapshot,
            key,
            value,
            PropertyAttributes::DEFAULT_DATA,
        )
    }

    /// Marks one own data property as deleted while retaining append-only shape metadata.
    pub(crate) fn delete_own_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<bool, ExecutionError> {
        let key = key.into();
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
            if self.is_function_prototype_property(receiver, key) {
                return Ok(false);
            }
            if self.is_function_metadata_property(receiver, key)? {
                self.add_property_slot(
                    object,
                    snapshot,
                    key,
                    Value::from_immediate(Immediate::Hole),
                    PropertyAttributes::data(false, false, true),
                )?;
            }
            return Ok(true);
        };
        if !property.attributes.configurable() {
            return Ok(false);
        }
        self.update_property_slot(
            snapshot,
            key,
            property.slot,
            Value::from_immediate(Immediate::Hole),
        )?;
        Ok(true)
    }

    /// Applies strict DeletePropertyOrThrow semantics to bytecode property deletion.
    pub(crate) fn delete_data_property_from_bytecode(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<bool, ExecutionError> {
        let deleted = self.delete_own_data_property(receiver, key)?;
        let strictness = self
            .fiber
            .frames
            .last()
            .expect("property deletion always has an active frame")
            .strictness;
        if !deleted && strictness == FunctionStrictness::Strict {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        Ok(deleted)
    }

    /// Mutates a fixed existing slot and publishes its potential young edge to the barrier.
    pub(super) fn update_property_slot(
        &mut self,
        snapshot: OrdinaryObject,
        key: PropertyKey,
        slot: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let storage = snapshot
            .storage
            .expect("an existing property slot always has storage");
        self.heap.with_running_scope(|scope| {
            let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let storage = no_gc
                    .borrow_mut(storage_local, self.types.property_storage)
                    .map_err(ExecutionError::NoGcBorrow)?;
                storage.slots[slot as usize] = value;
                storage.set_symbol_presence(
                    slot,
                    key,
                    value.as_immediate() != Some(Immediate::Hole),
                );
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(storage_local, value)
                .map_err(ExecutionError::HeapReference)?;
            if let Some(symbol) = key.symbol()
                && value.as_immediate() != Some(Immediate::Hole)
            {
                scope
                    .write_value_barrier(storage_local, symbol.value())
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Copies old slots into a traced pending backing, allocates it, then switches the object edge.
    pub(super) fn add_property_slot(
        &mut self,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: PropertyKey,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        self.add_property_slot_with_kind(
            object,
            snapshot,
            key,
            PropertyKind::Data,
            value,
            attributes,
        )
    }

    /// Publishes one kind-aware slot while preserving the compact shared Value backing.
    pub(super) fn add_property_slot_with_kind(
        &mut self,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: PropertyKey,
        kind: PropertyKind,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        let new_shape = self
            .shapes
            .transition_add_kind(snapshot.shape, key, kind, attributes)
            .map_err(ExecutionError::Shape)?;
        let new_length = self.shapes.property_count(new_shape) as usize;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(new_length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        let mut symbol_keys = Vec::new();
        if let Some(storage) = snapshot.storage {
            self.heap.with_running_scope(|scope| {
                let local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let old = no_gc
                        .borrow(local, self.types.property_storage)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    let symbol_key_count = old
                        .symbol_key_count()
                        .checked_add(usize::from(key.symbol().is_some()))
                        .ok_or(ExecutionError::PropertyStorageAllocationFailed)?;
                    symbol_keys
                        .try_reserve_exact(symbol_key_count)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                    slots.extend_from_slice(&old.slots);
                    old.append_symbol_keys(&mut symbol_keys);
                    Ok::<(), ExecutionError>(())
                })
            })?;
        } else if key.symbol().is_some() {
            symbol_keys
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        }
        slots.push(value);
        if let Some(symbol) = key.symbol() {
            symbol_keys.push(SymbolPropertyKey::new(
                u32::try_from(new_length - 1)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?,
                symbol,
                symbol.value(),
            ));
        }
        debug_assert_eq!(slots.len(), new_length);
        let (storage, receiver) = {
            let mut roots = PropertyMutationRoots {
                vm: VmRoots {
                    fiber: &mut self.fiber,
                    finalization_jobs: &mut self.finalization_jobs,
                    realm: &mut self.realm,
                    loaded_code: &mut self.loaded_code,
                },
                receiver: object.value(),
                value,
                symbol_key: key.symbol().map(SymbolId::value),
            };
            let storage = self
                .heap
                .try_allocate_external_with_gc(
                    self.types.property_storage,
                    0,
                    PropertyStorage::with_symbol_keys(
                        slots.into_boxed_slice(),
                        symbol_keys.into_boxed_slice(),
                    ),
                    AllocationSpace::Young,
                    &mut roots,
                )
                .map_err(ExecutionError::HeapAllocation)?;
            (storage, roots.receiver)
        };
        let (object, _) = self.object_snapshot(receiver)?;
        self.attach_property_storage(object, new_shape, storage)
    }

    /// Recovers the original Symbol value retained by one live own-key storage edge.
    #[cfg(test)]
    pub(crate) fn symbol_property_key_value(
        &mut self,
        snapshot: OrdinaryObject,
        symbol: SymbolId,
    ) -> Result<Option<Value>, ExecutionError> {
        let Some(property) = self
            .shapes
            .lookup(snapshot.shape, PropertyKey::Symbol(symbol))
        else {
            return Ok(None);
        };
        let Some(storage) = snapshot.storage else {
            return Ok(None);
        };
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.property_storage)
                    .map_err(ExecutionError::NoGcBorrow)
                    .map(|storage| storage.symbol_value(property.slot, symbol))
            })
        })
    }

    /// Resolves either ordinary or callable payloads to their shared ordinary-property snapshot.
    #[inline(always)]
    pub(crate) fn object_snapshot(
        &mut self,
        value: Value,
    ) -> Result<(ObjectReceiver, OrdinaryObject), ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        if let Ok(object) = self.heap.checked_reference(raw, self.types.ordinary_object) {
            let snapshot = self.heap.with_running_scope(|scope| {
                let local = scope.root(object).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.ordinary_object)
                        .copied()
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Ordinary(object), snapshot));
        }
        if let Ok(array) = self.heap.checked_reference(raw, self.types.array) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.array)
                        .map(|array| array.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Array(array), ordinary));
        }
        if let Ok(number) = self.heap.checked_reference(raw, self.types.number_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(number).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.number_object)
                        .map(|number| number.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Number(number), ordinary));
        }
        let function = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NotObject(value))?;
        let ordinary = self.heap.with_running_scope(|scope| {
            let local = scope.root(function).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(local, self.types.function)
                    .map(|function| function.ordinary)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok((ObjectReceiver::Function(function), ordinary))
    }

    /// Mutates the shared ordinary-object state for either object payload representation.
    pub(crate) fn set_object_extensible(
        &mut self,
        receiver: ObjectReceiver,
        extensible: bool,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Array(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(array, self.types.array)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Number(number) => self.heap.with_running_scope(|scope| {
                let number = scope.root(number).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(number, self.types.number_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
        }
    }

    /// Switches immutable shape metadata without touching the unchanged storage edge.
    pub(super) fn set_object_shape(
        &mut self,
        receiver: ObjectReceiver,
        shape: ShapeId,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Array(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(array, self.types.array)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Number(number) => self.heap.with_running_scope(|scope| {
                let number = scope.root(number).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(number, self.types.number_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
        }
    }

    /// Publishes a replacement storage edge through the receiver's concrete typed payload.
    fn attach_property_storage(
        &mut self,
        receiver: ObjectReceiver,
        shape: ShapeId,
        storage: GcRef<PropertyStorage>,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let object = no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    object.shape = shape;
                    object.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(object, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
            ObjectReceiver::Array(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let array = no_gc
                        .borrow_mut(array, self.types.array)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    array.ordinary.shape = shape;
                    array.ordinary.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(array, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let function = no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    function.ordinary.shape = shape;
                    function.ordinary.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(function, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
            ObjectReceiver::Number(number) => self.heap.with_running_scope(|scope| {
                let number = scope.root(number).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let number = no_gc
                        .borrow_mut(number, self.types.number_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    number.ordinary.shape = shape;
                    number.ordinary.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(number, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
        }
    }

    #[inline(always)]
    pub(crate) fn is_object_value(&self, value: Value) -> bool {
        let Some(raw) = value.as_heap_ref() else {
            return false;
        };
        self.heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
            || self.heap.checked_reference(raw, self.types.array).is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.number_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.function)
                .is_ok()
    }
}
