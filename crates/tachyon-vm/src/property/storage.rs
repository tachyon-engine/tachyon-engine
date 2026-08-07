//! Ordinary object slots, shapes, storage publication, and write barriers.

use super::{super::*, accessor::StoredProperty};
use crate::builtins::typed_array::TypedArrayIndex;

#[derive(Clone, Copy)]
struct RetainedProperty {
    key: PropertyKey,
    kind: PropertyKind,
    attributes: PropertyAttributes,
    value: Value,
}

struct CompactPropertyStorage {
    slots: Box<[Value]>,
    symbol_keys: Box<[SymbolPropertyKey]>,
}

impl Isolate {
    /// Returns the canonical Array index represented by one atom key.
    #[inline(always)]
    pub(crate) fn array_property_index(&self, key: PropertyKey) -> Option<u32> {
        key.atom().and_then(|atom| {
            self.atoms
                .get(atom)
                .and_then(|name| crate::property::keys::array_index(name.as_view()))
        })
    }

    /// Reads one present default dense Array property without consulting ordinary shape storage.
    #[inline(always)]
    pub(crate) fn dense_array_value(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let Some(index) = self.array_property_index(key) else {
            return Ok(None);
        };
        let Some(raw) = receiver.as_heap_ref() else {
            return Ok(None);
        };
        let Ok(array) = self.heap.checked_reference(raw, self.types.array) else {
            return Ok(None);
        };
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            let elements = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.array)
                    .map(|array| array.elements)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            let Some(elements) = elements else {
                return Ok(None);
            };
            let elements = scope.root(elements).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(elements, self.types.array_elements)
                    .map(|elements| elements.value(index))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Writes a default dense Array property, geometrically replacing fixed backing when needed.
    pub(crate) fn set_dense_array_value(
        &mut self,
        receiver: Value,
        index: u32,
        value: Value,
    ) -> Result<bool, ExecutionError> {
        if index > tuning::arrays::MAX_DENSE_ELEMENT_INDEX {
            return Ok(false);
        }
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let Ok(array) = self.heap.checked_reference(raw, self.types.array) else {
            return Ok(false);
        };
        let elements = self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.array)
                    .map(|array| array.elements)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if elements.is_none() && (index as usize) >= tuning::arrays::INITIAL_DENSE_ELEMENT_CAPACITY
        {
            return Ok(false);
        }
        if let Some(elements) = elements {
            let capacity = self.heap.with_running_scope(|scope| {
                let elements = scope.root(elements).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(elements, self.types.array_elements)
                        .map(|elements| elements.capacity())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            if (index as usize).saturating_sub(capacity) > tuning::arrays::MAX_DENSE_GROWTH_GAP {
                return Ok(false);
            }
            if (index as usize) < capacity {
                self.heap.with_running_scope(|scope| {
                    let elements = scope.root(elements).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(elements, self.types.array_elements)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .set(index, value)
                            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)
                    })?;
                    scope
                        .write_value_barrier(elements, value)
                        .map_err(ExecutionError::HeapReference)
                })?;
                return Ok(true);
            }
        }
        self.grow_dense_array_value(receiver, elements, index, value)?;
        Ok(true)
    }

    /// Allocates, initializes, and publishes one larger exactly accounted dense backing.
    fn grow_dense_array_value(
        &mut self,
        receiver: Value,
        previous: Option<GcRef<ArrayElements>>,
        index: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let current_capacity = if let Some(previous) = previous {
            self.heap.with_running_scope(|scope| {
                let previous = scope.root(previous).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(previous, self.types.array_elements)
                        .map(|elements| elements.capacity())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?
        } else {
            0
        };
        let required = (index as usize)
            .checked_add(1)
            .ok_or(ExecutionError::PropertyStorageAllocationFailed)?;
        let capacity = tuning::arrays::grown_dense_element_capacity(current_capacity, required)
            .ok_or(ExecutionError::PropertyStorageAllocationFailed)?;
        let mut grown = if let Some(previous) = previous {
            self.heap.with_running_scope(|scope| {
                let previous = scope.root(previous).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(previous, self.types.array_elements)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .grow_copy(capacity)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)
                })
            })?
        } else {
            ArrayElements::with_capacity(capacity)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?
        };
        grown
            .set(index, value)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        let mut roots = PropertyMutationRoots {
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
            receiver,
            value,
            symbol_key: None,
        };
        let elements = self
            .heap
            .try_allocate_external_with_gc(
                self.types.array_elements,
                0,
                grown,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let raw = roots
            .receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(roots.receiver))?;
        let array = self
            .heap
            .checked_reference(raw, self.types.array)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            let elements = scope.root(elements).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(array, self.types.array)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .elements = Some(elements.as_gc_ref());
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_barrier(array, elements)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Turns one present dense index into a hole and reports whether it was present.
    pub(super) fn delete_dense_array_value(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<bool, ExecutionError> {
        let Some(index) = self.array_property_index(key) else {
            return Ok(false);
        };
        let Some(raw) = receiver.as_heap_ref() else {
            return Ok(false);
        };
        let Ok(array) = self.heap.checked_reference(raw, self.types.array) else {
            return Ok(false);
        };
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            let elements = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.array)
                    .map(|array| array.elements)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            let Some(elements) = elements else {
                return Ok(false);
            };
            let elements = scope.root(elements).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(elements, self.types.array_elements)
                    .map(|elements| elements.delete(index))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Snapshots present dense indices so atom interning never overlaps a GC backing borrow.
    pub(crate) fn dense_array_indices(
        &mut self,
        receiver: Value,
    ) -> Result<Vec<u32>, ExecutionError> {
        let Some(raw) = receiver.as_heap_ref() else {
            return Ok(Vec::new());
        };
        let Ok(array) = self.heap.checked_reference(raw, self.types.array) else {
            return Ok(Vec::new());
        };
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            let elements = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.array)
                    .map(|array| array.elements)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            let Some(elements) = elements else {
                return Ok(Vec::new());
            };
            let elements = scope.root(elements).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let elements = no_gc
                    .borrow(elements, self.types.array_elements)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut indices = Vec::new();
                indices
                    .try_reserve_exact(elements.present_count() as usize)
                    .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
                indices.extend(elements.present_indices());
                Ok(indices)
            })
        })
    }

    /// Snapshots only a fully packed dense backing for an engine-private spread-call fast path.
    pub(crate) fn packed_array_values(
        &mut self,
        receiver: Value,
    ) -> Result<Option<Vec<Value>>, ExecutionError> {
        let Some(raw) = receiver.as_heap_ref() else {
            return Ok(None);
        };
        let Ok(array) = self.heap.checked_reference(raw, self.types.array) else {
            return Ok(None);
        };
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            let elements = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.array)
                    .map(|array| array.elements)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            let Some(elements) = elements else {
                return Ok(Some(Vec::new()));
            };
            let elements = scope.root(elements).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(elements, self.types.array_elements)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .copy_packed_values()
                    .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)
            })
        })
    }

    /// Clears default dense properties after ordinary non-configurable length checks succeed.
    pub(super) fn truncate_dense_array(
        &mut self,
        receiver: Value,
        length: u32,
    ) -> Result<(), ExecutionError> {
        let Some(raw) = receiver.as_heap_ref() else {
            return Ok(());
        };
        let Ok(array) = self.heap.checked_reference(raw, self.types.array) else {
            return Ok(());
        };
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            let elements = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.array)
                    .map(|array| array.elements)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            let Some(elements) = elements else {
                return Ok(());
            };
            let elements = scope.root(elements).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(elements, self.types.array_elements)
                    .map(|elements| elements.truncate(length))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Applies OrdinarySetPrototypeOf for ordinary-object payloads without allocating or invoking JS.
    pub(crate) fn ordinary_set_prototype_of(
        &mut self,
        target: Value,
        prototype: Value,
    ) -> Result<bool, ExecutionError> {
        if prototype.as_immediate() != Some(Immediate::Null) && !self.is_object_value(prototype) {
            return Err(ExecutionError::NotObject(prototype));
        }
        let (receiver, snapshot) = self.object_snapshot(target)?;
        if snapshot.prototype == prototype {
            return Ok(true);
        }
        if self.realm.object_prototype == Some(target) {
            return Ok(false);
        }
        if !snapshot.extensible {
            return Ok(false);
        }
        let mut candidate = prototype;
        while self.is_object_value(candidate) {
            if candidate == target {
                return Ok(false);
            }
            if self.is_proxy_value(candidate) {
                break;
            }
            candidate = self.object_snapshot(candidate)?.1.prototype;
        }
        match receiver {
            ObjectReceiver::Ordinary(object) => {
                self.heap.with_running_scope(|scope| {
                    let object = scope.root(object).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(object, self.types.ordinary_object)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .prototype = prototype;
                        Ok::<(), ExecutionError>(())
                    })?;
                    scope
                        .write_value_barrier(object, prototype)
                        .map_err(ExecutionError::HeapReference)
                })?;
            }
            ObjectReceiver::ModuleNamespace(_) => return Ok(false),
            ObjectReceiver::Function(_) => {
                self.set_function_internal_prototype(target, prototype)?;
            }
            ObjectReceiver::Array(array) => {
                self.heap.with_running_scope(|scope| {
                    let array = scope.root(array).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(array, self.types.array)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .ordinary
                            .prototype = prototype;
                        Ok::<(), ExecutionError>(())
                    })?;
                    scope
                        .write_value_barrier(array, prototype)
                        .map_err(ExecutionError::HeapReference)
                })?;
            }
            ObjectReceiver::IteratorHelper(iterator) => {
                self.set_embedded_object_prototype(
                    iterator,
                    self.types.iterator_helper,
                    prototype,
                    |object| &mut object.ordinary,
                )?;
            }
            ObjectReceiver::WrapForValidIterator(iterator) => {
                self.set_embedded_object_prototype(
                    iterator,
                    self.types.wrap_for_valid_iterator,
                    prototype,
                    |object| &mut object.ordinary,
                )?;
            }
            ObjectReceiver::IntlCollator(collator) => {
                self.set_embedded_object_prototype(
                    collator,
                    self.types.intl_collator_object,
                    prototype,
                    |object| &mut object.ordinary,
                )?;
            }
            ObjectReceiver::IntlLocale(locale) => {
                self.set_embedded_object_prototype(
                    locale,
                    self.types.intl_locale_object,
                    prototype,
                    |object| &mut object.ordinary,
                )?;
            }
            ObjectReceiver::IntlListFormat(list_format) => {
                self.set_embedded_object_prototype(
                    list_format,
                    self.types.intl_list_format_object,
                    prototype,
                    |object| &mut object.ordinary,
                )?;
            }
            ObjectReceiver::IntlDateTimeFormat(date_time_format) => {
                self.set_embedded_object_prototype(
                    date_time_format,
                    self.types.intl_date_time_format_object,
                    prototype,
                    |object| &mut object.ordinary,
                )?;
            }
            ObjectReceiver::IntlNumberFormat(number_format) => {
                self.set_embedded_object_prototype(
                    number_format,
                    self.types.intl_number_format_object,
                    prototype,
                    |object| &mut object.ordinary,
                )?;
            }
            _ => return Err(ExecutionError::UnsupportedAccessorDescriptor),
        }
        Ok(true)
    }

    /// Checks one resolved slot's tombstone state without interpreting its data/accessor payload.
    pub(crate) fn property_is_present_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        property: PropertyLookup,
    ) -> Result<bool, ExecutionError> {
        self.raw_property_value_from_snapshot(snapshot, property)
            .map(|value| value.is_some())
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
        self.sync_mapped_argument(receiver, key, value)?;
        if self.is_array_value(receiver)? && key == PropertyKey::Atom(self.length_atom()?) {
            return self.set_array_length_value(receiver, value);
        }
        if self.is_function_prototype_property(receiver, key) {
            if self.has_read_only_prototype(receiver)? {
                return Err(ExecutionError::ReadOnlyProperty(receiver));
            }
            self.intrinsic_property_atoms.prototype = key.atom();
            return self.set_function_prototype(receiver, value);
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let dense_index = self.array_property_index(key);
        if let Some(index) = dense_index
            && self.dense_array_value(receiver, key)?.is_some()
        {
            self.set_dense_array_value(receiver, index, value)?;
            self.grow_array_length_for_index_property(receiver, key)?;
            return Ok(());
        }
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
                None => {
                    if let Some(index) = dense_index
                        && self.set_dense_array_value(receiver, index, value)?
                    {
                        self.grow_array_length_for_index_property(receiver, key)?;
                        return Ok(());
                    }
                }
            }
            if !snapshot.extensible {
                return Err(ExecutionError::NonExtensibleObject(receiver));
            }
            self.remove_property_slot(object, snapshot, key)?;
            let (object, snapshot) = self.object_snapshot(receiver)?;
            self.add_property_slot(
                object,
                snapshot,
                key,
                value,
                PropertyAttributes::DEFAULT_DATA,
            )?;
            self.grow_array_length_for_index_property(receiver, key)?;
            return Ok(());
        }
        if self.is_function_metadata_property(receiver, key)? {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        if !snapshot.extensible {
            return Err(ExecutionError::NonExtensibleObject(receiver));
        }
        if let Some(index) = dense_index
            && self.set_dense_array_value(receiver, index, value)?
        {
            self.grow_array_length_for_index_property(receiver, key)?;
            return Ok(());
        }
        self.add_property_slot(
            object,
            snapshot,
            key,
            value,
            PropertyAttributes::DEFAULT_DATA,
        )?;
        self.grow_array_length_for_index_property(receiver, key)
    }

    /// Grows an Array length after publishing a new canonical array-index property.
    pub(crate) fn grow_array_length_for_index_property(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        if !self.is_array_value(receiver)? {
            return Ok(());
        }
        let Some(atom) = key.atom() else {
            return Ok(());
        };
        let Some(index) = self
            .atoms
            .get(atom)
            .and_then(|name| crate::property::keys::array_index(name.as_view()))
        else {
            return Ok(());
        };
        let length_key = PropertyKey::Atom(self.length_atom()?);
        let current_length = self
            .complete_own_property_descriptor(receiver, length_key)?
            .and_then(|descriptor| match descriptor {
                PropertyDescriptor::Data(data) => data.value.and_then(numeric_value),
                _ => None,
            })
            .unwrap_or(0.0);
        let next_length = f64::from(index) + 1.0;
        if next_length > current_length {
            self.set_own_data_property(
                receiver,
                length_key,
                safe_integer_value(next_length as u64),
            )?;
        }
        Ok(())
    }

    /// Defines a fresh ordinary data slot with the intrinsic attributes required by an exotic.
    pub(crate) fn define_fresh_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        let key = key.into();
        let (object, snapshot) = self.object_snapshot(receiver)?;
        if self.shapes.lookup(snapshot.shape, key).is_some() {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        if !snapshot.extensible {
            return Err(ExecutionError::NonExtensibleObject(receiver));
        }
        self.add_property_slot(object, snapshot, key, value, attributes)
    }

    /// Marks one own data property as deleted while retaining append-only shape metadata.
    pub(crate) fn delete_own_data_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> Result<bool, ExecutionError> {
        let key = key.into();
        if self.is_module_namespace_value(receiver)
            && self.module_namespace_has_export(receiver, key)?
        {
            return Ok(false);
        }
        if self.is_typed_array_value(receiver) {
            match self.typed_array_index(key)? {
                TypedArrayIndex::Valid(index) => {
                    return Ok(index >= self.typed_array_snapshot(receiver)?.length);
                }
                TypedArrayIndex::Invalid => return Ok(true),
                TypedArrayIndex::NonNumeric => {}
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
                return Ok(false);
            }
        }
        if self.delete_dense_array_value(receiver, key)? {
            return Ok(true);
        }
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
        if self
            .raw_property_value_from_snapshot(snapshot, property)?
            .is_none()
        {
            return Ok(true);
        }
        let suppress_virtual = self.is_function_metadata_property(receiver, key)?;
        self.remove_property_slot(object, snapshot, key)?;
        if suppress_virtual {
            let (object, snapshot) = self.object_snapshot(receiver)?;
            self.add_property_slot(
                object,
                snapshot,
                key,
                Value::from_immediate(Immediate::Hole),
                PropertyAttributes::data(false, false, true),
            )?;
        }
        Ok(true)
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
            if let Some(symbol) = key.symbol_identity()
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
                        .checked_add(usize::from(key.symbol_identity().is_some()))
                        .ok_or(ExecutionError::PropertyStorageAllocationFailed)?;
                    symbol_keys
                        .try_reserve_exact(symbol_key_count)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                    slots.extend_from_slice(&old.slots);
                    old.append_symbol_keys(&mut symbol_keys);
                    Ok::<(), ExecutionError>(())
                })
            })?;
        } else if key.symbol_identity().is_some() {
            symbol_keys
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        }
        slots.push(value);
        if let Some(symbol) = key.symbol_identity() {
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
                    suspended_fibers: &mut self.suspended_fibers,
                    finalization_jobs: &mut self.finalization_jobs,
                    promise_jobs: &mut self.promise_jobs,
                    realm: &mut self.realm,
                    inactive_realms: &mut self.inactive_realms,
                    loaded_code: &mut self.loaded_code,
                    module_graph: &mut self.module_graph,
                },
                receiver: object.value(),
                value,
                symbol_key: key.symbol_identity().map(SymbolId::value),
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
        self.replace_property_storage(object, new_shape, Some(storage))
    }

    /// Removes one structural property and publishes an exactly compacted replacement backing.
    pub(super) fn remove_property_slot(
        &mut self,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        removed: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let storage = snapshot
            .storage
            .expect("a structural property always owns storage");
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        let retained_count = keys.len().saturating_sub(1);
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(retained_count)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let storage = no_gc
                    .borrow(storage, self.types.property_storage)
                    .map_err(ExecutionError::NoGcBorrow)?;
                for key in keys {
                    if key == removed {
                        continue;
                    }
                    let property = self
                        .shapes
                        .lookup(snapshot.shape, key)
                        .expect("structural key resolves in its source shape");
                    retained.push(RetainedProperty {
                        key,
                        kind: property.kind,
                        attributes: property.attributes,
                        value: storage.slots[property.slot as usize],
                    });
                }
                Ok::<(), ExecutionError>(())
            })
        })?;
        debug_assert_eq!(retained.len(), retained_count);
        let new_shape = self.rebuild_shape(&retained)?;
        if retained.is_empty() {
            self.replace_property_storage(object, new_shape, None)?;
            return Ok(());
        }
        let compact = compact_property_storage(&retained)?;
        let receiver = object.value();
        let (storage, receiver) = {
            let mut roots = PropertyMutationRoots {
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
                receiver,
                value: receiver,
                symbol_key: None,
            };
            let storage = self
                .heap
                .try_allocate_external_with_gc(
                    self.types.property_storage,
                    0,
                    PropertyStorage::with_symbol_keys(compact.slots, compact.symbol_keys),
                    AllocationSpace::Young,
                    &mut roots,
                )
                .map_err(ExecutionError::HeapAllocation)?;
            (storage, roots.receiver)
        };
        let (object, _) = self.object_snapshot(receiver)?;
        self.replace_property_storage(object, new_shape, Some(storage))?;
        Ok(())
    }

    /// Replays retained descriptors from the root so removal cannot leave stale lookup overlays.
    fn rebuild_shape(&mut self, retained: &[RetainedProperty]) -> Result<ShapeId, ExecutionError> {
        let mut shape = ShapeId::EMPTY;
        for property in retained {
            shape = self
                .shapes
                .transition_add_kind(shape, property.key, property.kind, property.attributes)
                .map_err(ExecutionError::Shape)?;
        }
        Ok(shape)
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
        if let Ok(arguments) = self
            .heap
            .checked_reference(raw, self.types.arguments_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let arguments = scope.root(arguments).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(arguments, self.types.arguments_object)
                        .map(|arguments| arguments.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Arguments(arguments), ordinary));
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
        if let Ok(buffer) = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(buffer).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.array_buffer_object)
                        .map(|buffer| buffer.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::ArrayBuffer(buffer), ordinary));
        }
        if let Ok(view) = self
            .heap
            .checked_reference(raw, self.types.data_view_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(view).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.data_view_object)
                        .map(|view| view.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::DataView(view), ordinary));
        }
        if let Ok(array) = self
            .heap
            .checked_reference(raw, self.types.typed_array_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.typed_array_object)
                        .map(|array| array.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::TypedArray(array), ordinary));
        }
        if let Ok(signal) = self.heap.checked_reference(raw, self.types.signal_state) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(signal, self.types.signal_state)
                        .map(|signal| signal.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::SignalState(signal), ordinary));
        }
        if let Ok(signal) = self.heap.checked_reference(raw, self.types.signal_computed) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(signal, self.types.signal_computed)
                        .map(|signal| signal.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::SignalComputed(signal), ordinary));
        }
        if let Ok(signal) = self.heap.checked_reference(raw, self.types.signal_watcher) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(signal, self.types.signal_watcher)
                        .map(|signal| signal.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::SignalWatcher(signal), ordinary));
        }
        if let Ok(generator) = self
            .heap
            .checked_reference(raw, self.types.generator_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let generator = scope.root(generator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(generator, self.types.generator_object)
                        .map(|generator| generator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Generator(generator), ordinary));
        }
        if let Ok(iterator) = self
            .heap
            .checked_reference(raw, self.types.async_from_sync_iterator)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(iterator, self.types.async_from_sync_iterator)
                        .map(|iterator| iterator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::AsyncFromSyncIterator(iterator), ordinary));
        }
        if let Ok(iterator) = self.heap.checked_reference(raw, self.types.iterator_helper) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(iterator, self.types.iterator_helper)
                        .map(|iterator| iterator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::IteratorHelper(iterator), ordinary));
        }
        if let Ok(iterator) = self
            .heap
            .checked_reference(raw, self.types.wrap_for_valid_iterator)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(iterator, self.types.wrap_for_valid_iterator)
                        .map(|iterator| iterator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::WrapForValidIterator(iterator), ordinary));
        }
        if let Ok(date) = self.heap.checked_reference(raw, self.types.date_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(date).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.date_object)
                        .map(|date| date.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Date(date), ordinary));
        }
        if let Ok(collator) = self
            .heap
            .checked_reference(raw, self.types.intl_collator_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let collator = scope.root(collator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(collator, self.types.intl_collator_object)
                        .map(|collator| collator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::IntlCollator(collator), ordinary));
        }
        if let Ok(locale) = self
            .heap
            .checked_reference(raw, self.types.intl_locale_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let locale = scope.root(locale).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(locale, self.types.intl_locale_object)
                        .map(|locale| locale.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::IntlLocale(locale), ordinary));
        }
        if let Ok(list_format) = self
            .heap
            .checked_reference(raw, self.types.intl_list_format_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let list_format = scope.root(list_format).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(list_format, self.types.intl_list_format_object)
                        .map(|list_format| list_format.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::IntlListFormat(list_format), ordinary));
        }
        if let Ok(number_format) = self
            .heap
            .checked_reference(raw, self.types.intl_number_format_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let number_format = scope.root(number_format).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(number_format, self.types.intl_number_format_object)
                        .map(|number_format| number_format.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::IntlNumberFormat(number_format), ordinary));
        }
        if let Ok(date_time_format) = self
            .heap
            .checked_reference(raw, self.types.intl_date_time_format_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let date_time_format =
                    scope.root(date_time_format).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(date_time_format, self.types.intl_date_time_format_object)
                        .map(|date_time_format| date_time_format.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((
                ObjectReceiver::IntlDateTimeFormat(date_time_format),
                ordinary,
            ));
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
        if let Ok(bigint) = self.heap.checked_reference(raw, self.types.bigint_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(bigint).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.bigint_object)
                        .map(|bigint| bigint.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::BigInt(bigint), ordinary));
        }
        if let Ok(boolean) = self.heap.checked_reference(raw, self.types.boolean_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(boolean).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.boolean_object)
                        .map(|boolean| boolean.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Boolean(boolean), ordinary));
        }
        if let Ok(string) = self.heap.checked_reference(raw, self.types.string_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.string_object)
                        .map(|string| string.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::String(string), ordinary));
        }
        if let Ok(symbol) = self.heap.checked_reference(raw, self.types.symbol_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(symbol, self.types.symbol_object)
                        .map(|symbol| symbol.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Symbol(symbol), ordinary));
        }
        if let Ok(regexp) = self.heap.checked_reference(raw, self.types.regexp_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let regexp = scope.root(regexp).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(regexp, self.types.regexp_object)
                        .map(|regexp| regexp.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::RegExp(regexp), ordinary));
        }
        if let Ok(iterator) = self
            .heap
            .checked_reference(raw, self.types.regexp_string_iterator)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(iterator, self.types.regexp_string_iterator)
                        .map(|iterator| iterator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::RegExpStringIterator(iterator), ordinary));
        }
        if let Ok(promise) = self.heap.checked_reference(raw, self.types.promise_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let promise = scope.root(promise).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(promise, self.types.promise_object)
                        .map(|promise| promise.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Promise(promise), ordinary));
        }
        if let Ok(map) = self.heap.checked_reference(raw, self.types.map_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(map, self.types.map_object)
                        .map(|map| map.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Map(map), ordinary));
        }
        if let Ok(set) = self.heap.checked_reference(raw, self.types.set_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(set, self.types.set_object)
                        .map(|set| set.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Set(set), ordinary));
        }
        if let Ok(map) = self.heap.checked_reference(raw, self.types.weak_map_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(map, self.types.weak_map_object)
                        .map(|map| map.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::WeakMap(map), ordinary));
        }
        if let Ok(set) = self.heap.checked_reference(raw, self.types.weak_set_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(set, self.types.weak_set_object)
                        .map(|set| set.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::WeakSet(set), ordinary));
        }
        if let Ok(reference) = self.heap.checked_reference(raw, self.types.weak_ref_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let reference = scope.root(reference).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(reference, self.types.weak_ref_object)
                        .map(|reference| reference.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::WeakRef(reference), ordinary));
        }
        if let Ok(registry) = self
            .heap
            .checked_reference(raw, self.types.finalization_registry_object)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let registry = scope.root(registry).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(registry, self.types.finalization_registry_object)
                        .map(|registry| registry.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::FinalizationRegistry(registry), ordinary));
        }
        if let Ok(iterator) = self.heap.checked_reference(raw, self.types.array_iterator) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.array_iterator)
                        .map(|iterator| iterator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::ArrayIterator(iterator), ordinary));
        }
        if let Ok(iterator) = self
            .heap
            .checked_reference(raw, self.types.collection_iterator)
        {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.collection_iterator)
                        .map(|iterator| iterator.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::CollectionIterator(iterator), ordinary));
        }
        if let Ok(error) = self.heap.checked_reference(raw, self.types.error_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let error = scope.root(error).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(error, self.types.error_object)
                        .map(|error| error.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Error(error), ordinary));
        }
        if let Ok(function) = self.heap.checked_reference(raw, self.types.function) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(function).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.function)
                        .map(|function| function.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Function(function), ordinary));
        }
        if let Ok(namespace) = self
            .heap
            .checked_reference(raw, self.types.module_namespace_object)
        {
            let ordinary = self.module_namespace_ordinary(value)?;
            return Ok((ObjectReceiver::ModuleNamespace(namespace), ordinary));
        }
        Err(ExecutionError::NotObject(value))
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
            ObjectReceiver::ModuleNamespace(namespace) => self.heap.with_running_scope(|scope| {
                let namespace = scope.root(namespace).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(namespace, self.types.module_namespace_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Arguments(arguments) => self.heap.with_running_scope(|scope| {
                let arguments = scope.root(arguments).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(arguments, self.types.arguments_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
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
            ObjectReceiver::Generator(generator) => self.heap.with_running_scope(|scope| {
                let generator = scope.root(generator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(generator, self.types.generator_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Error(error) => self.heap.with_running_scope(|scope| {
                let error = scope.root(error).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(error, self.types.error_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::ArrayBuffer(buffer) => self.heap.with_running_scope(|scope| {
                let buffer = scope.root(buffer).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(buffer, self.types.array_buffer_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::DataView(view) => self.heap.with_running_scope(|scope| {
                let view = scope.root(view).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(view, self.types.data_view_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::TypedArray(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(array, self.types.typed_array_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::SignalState(signal) => self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(signal, self.types.signal_state)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::SignalComputed(signal) => self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(signal, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::SignalWatcher(signal) => self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(signal, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Date(date) => self.heap.with_running_scope(|scope| {
                let date = scope.root(date).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(date, self.types.date_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::IntlCollator(collator) => self.set_embedded_object_extensible(
                collator,
                self.types.intl_collator_object,
                extensible,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlLocale(locale) => self.set_embedded_object_extensible(
                locale,
                self.types.intl_locale_object,
                extensible,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlListFormat(list_format) => self.set_embedded_object_extensible(
                list_format,
                self.types.intl_list_format_object,
                extensible,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlDateTimeFormat(date_time_format) => self
                .set_embedded_object_extensible(
                    date_time_format,
                    self.types.intl_date_time_format_object,
                    extensible,
                    |object| &mut object.ordinary,
                ),
            ObjectReceiver::IntlNumberFormat(number_format) => self.set_embedded_object_extensible(
                number_format,
                self.types.intl_number_format_object,
                extensible,
                |object| &mut object.ordinary,
            ),
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
            ObjectReceiver::BigInt(bigint) => self.heap.with_running_scope(|scope| {
                let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(bigint, self.types.bigint_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Boolean(boolean) => self.heap.with_running_scope(|scope| {
                let boolean = scope.root(boolean).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(boolean, self.types.boolean_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::String(string) => self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(string, self.types.string_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Symbol(symbol) => self.heap.with_running_scope(|scope| {
                let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(symbol, self.types.symbol_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::RegExp(regexp) => self.heap.with_running_scope(|scope| {
                let regexp = scope.root(regexp).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(regexp, self.types.regexp_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::RegExpStringIterator(iterator) => {
                self.heap.with_running_scope(|scope| {
                    let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(iterator, self.types.regexp_string_iterator)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .ordinary
                            .extensible = extensible;
                        Ok(())
                    })
                })
            }
            ObjectReceiver::Promise(promise) => self.heap.with_running_scope(|scope| {
                let promise = scope.root(promise).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(promise, self.types.promise_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Map(map) => self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(map, self.types.map_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Set(set) => self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(set, self.types.set_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::WeakMap(map) => self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(map, self.types.weak_map_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::WeakSet(set) => self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(set, self.types.weak_set_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::WeakRef(reference) => self.heap.with_running_scope(|scope| {
                let reference = scope.root(reference).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(reference, self.types.weak_ref_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::FinalizationRegistry(registry) => {
                self.heap.with_running_scope(|scope| {
                    let registry = scope.root(registry).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(registry, self.types.finalization_registry_object)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .ordinary
                            .extensible = extensible;
                        Ok(())
                    })
                })
            }
            ObjectReceiver::ArrayIterator(iterator) => self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(iterator, self.types.array_iterator)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::AsyncFromSyncIterator(iterator) => {
                self.heap.with_running_scope(|scope| {
                    let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(iterator, self.types.async_from_sync_iterator)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .ordinary
                            .extensible = extensible;
                        Ok(())
                    })
                })
            }
            ObjectReceiver::IteratorHelper(iterator) => self.set_embedded_object_extensible(
                iterator,
                self.types.iterator_helper,
                extensible,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::WrapForValidIterator(iterator) => self.set_embedded_object_extensible(
                iterator,
                self.types.wrap_for_valid_iterator,
                extensible,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::CollectionIterator(iterator) => self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(iterator, self.types.collection_iterator)
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
            ObjectReceiver::ModuleNamespace(namespace) => self.heap.with_running_scope(|scope| {
                let namespace = scope.root(namespace).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(namespace, self.types.module_namespace_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Arguments(arguments) => self.heap.with_running_scope(|scope| {
                let arguments = scope.root(arguments).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(arguments, self.types.arguments_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
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
            ObjectReceiver::Generator(generator) => self.heap.with_running_scope(|scope| {
                let generator = scope.root(generator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(generator, self.types.generator_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Error(error) => self.heap.with_running_scope(|scope| {
                let error = scope.root(error).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(error, self.types.error_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::ArrayBuffer(buffer) => self.heap.with_running_scope(|scope| {
                let buffer = scope.root(buffer).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(buffer, self.types.array_buffer_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::DataView(view) => self.heap.with_running_scope(|scope| {
                let view = scope.root(view).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(view, self.types.data_view_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::TypedArray(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(array, self.types.typed_array_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::SignalState(signal) => self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(signal, self.types.signal_state)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::SignalComputed(signal) => self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(signal, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::SignalWatcher(signal) => self.heap.with_running_scope(|scope| {
                let signal = scope.root(signal).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(signal, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Date(date) => self.heap.with_running_scope(|scope| {
                let date = scope.root(date).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(date, self.types.date_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::IntlCollator(collator) => self.set_embedded_object_shape(
                collator,
                self.types.intl_collator_object,
                shape,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlLocale(locale) => self.set_embedded_object_shape(
                locale,
                self.types.intl_locale_object,
                shape,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlListFormat(list_format) => self.set_embedded_object_shape(
                list_format,
                self.types.intl_list_format_object,
                shape,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlDateTimeFormat(date_time_format) => self.set_embedded_object_shape(
                date_time_format,
                self.types.intl_date_time_format_object,
                shape,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlNumberFormat(number_format) => self.set_embedded_object_shape(
                number_format,
                self.types.intl_number_format_object,
                shape,
                |object| &mut object.ordinary,
            ),
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
            ObjectReceiver::BigInt(bigint) => self.heap.with_running_scope(|scope| {
                let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(bigint, self.types.bigint_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Boolean(boolean) => self.heap.with_running_scope(|scope| {
                let boolean = scope.root(boolean).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(boolean, self.types.boolean_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::String(string) => self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(string, self.types.string_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Symbol(symbol) => self.heap.with_running_scope(|scope| {
                let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(symbol, self.types.symbol_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::RegExp(regexp) => self.heap.with_running_scope(|scope| {
                let regexp = scope.root(regexp).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(regexp, self.types.regexp_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::RegExpStringIterator(iterator) => {
                self.heap.with_running_scope(|scope| {
                    let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(iterator, self.types.regexp_string_iterator)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .ordinary
                            .shape = shape;
                        Ok(())
                    })
                })
            }
            ObjectReceiver::Promise(promise) => self.heap.with_running_scope(|scope| {
                let promise = scope.root(promise).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(promise, self.types.promise_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Map(map) => self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(map, self.types.map_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Set(set) => self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(set, self.types.set_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::WeakMap(map) => self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(map, self.types.weak_map_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::WeakSet(set) => self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(set, self.types.weak_set_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::WeakRef(reference) => self.heap.with_running_scope(|scope| {
                let reference = scope.root(reference).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(reference, self.types.weak_ref_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::FinalizationRegistry(registry) => {
                self.heap.with_running_scope(|scope| {
                    let registry = scope.root(registry).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(registry, self.types.finalization_registry_object)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .ordinary
                            .shape = shape;
                        Ok(())
                    })
                })
            }
            ObjectReceiver::ArrayIterator(iterator) => self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(iterator, self.types.array_iterator)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::AsyncFromSyncIterator(iterator) => {
                self.heap.with_running_scope(|scope| {
                    let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_mut(iterator, self.types.async_from_sync_iterator)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .ordinary
                            .shape = shape;
                        Ok(())
                    })
                })
            }
            ObjectReceiver::IteratorHelper(iterator) => self.set_embedded_object_shape(
                iterator,
                self.types.iterator_helper,
                shape,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::WrapForValidIterator(iterator) => self.set_embedded_object_shape(
                iterator,
                self.types.wrap_for_valid_iterator,
                shape,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::CollectionIterator(iterator) => self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(iterator, self.types.collection_iterator)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
        }
    }

    /// Publishes a replacement storage edge through the receiver's concrete typed payload.
    fn replace_property_storage(
        &mut self,
        receiver: ObjectReceiver,
        shape: ShapeId,
        storage: Option<GcRef<PropertyStorage>>,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let object = no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    object.shape = shape;
                    object.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(object, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::ModuleNamespace(namespace) => self.heap.with_running_scope(|scope| {
                let namespace = scope.root(namespace).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let namespace = no_gc
                        .borrow_mut(namespace, self.types.module_namespace_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    namespace.ordinary.shape = shape;
                    namespace.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(namespace, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Arguments(arguments) => self.heap.with_running_scope(|scope| {
                let arguments = scope.root(arguments).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let arguments = no_gc
                        .borrow_mut(arguments, self.types.arguments_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    arguments.ordinary.shape = shape;
                    arguments.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(arguments, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Array(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let array = no_gc
                        .borrow_mut(array, self.types.array)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    array.ordinary.shape = shape;
                    array.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(array, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let function = no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    function.ordinary.shape = shape;
                    function.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(function, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Generator(generator) => self.heap.with_running_scope(|scope| {
                let generator = scope.root(generator).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let generator = no_gc
                        .borrow_mut(generator, self.types.generator_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    generator.ordinary.shape = shape;
                    generator.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(generator, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Error(error) => self.heap.with_running_scope(|scope| {
                let error = scope.root(error).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let error = no_gc
                        .borrow_mut(error, self.types.error_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    error.ordinary.shape = shape;
                    error.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(error, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::ArrayBuffer(buffer) => self.heap.with_running_scope(|scope| {
                let buffer = scope.root(buffer).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let buffer = no_gc
                        .borrow_mut(buffer, self.types.array_buffer_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    buffer.ordinary.shape = shape;
                    buffer.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(buffer, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::DataView(view) => self.heap.with_running_scope(|scope| {
                let view = scope.root(view).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let view = no_gc
                        .borrow_mut(view, self.types.data_view_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    view.ordinary.shape = shape;
                    view.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(view, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::TypedArray(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let array = no_gc
                        .borrow_mut(array, self.types.typed_array_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    array.ordinary.shape = shape;
                    array.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(array, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::SignalState(signal) => self.update_embedded_object_storage(
                signal,
                self.types.signal_state,
                shape,
                storage,
                |node| &mut node.ordinary,
            ),
            ObjectReceiver::SignalComputed(signal) => self.update_embedded_object_storage(
                signal,
                self.types.signal_computed,
                shape,
                storage,
                |node| &mut node.ordinary,
            ),
            ObjectReceiver::SignalWatcher(signal) => self.update_embedded_object_storage(
                signal,
                self.types.signal_watcher,
                shape,
                storage,
                |node| &mut node.ordinary,
            ),
            ObjectReceiver::Date(date) => self.heap.with_running_scope(|scope| {
                let date = scope.root(date).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let date = no_gc
                        .borrow_mut(date, self.types.date_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    date.ordinary.shape = shape;
                    date.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(date, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::IntlCollator(collator) => self.update_embedded_object_storage(
                collator,
                self.types.intl_collator_object,
                shape,
                storage,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlLocale(locale) => self.update_embedded_object_storage(
                locale,
                self.types.intl_locale_object,
                shape,
                storage,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlListFormat(list_format) => self.update_embedded_object_storage(
                list_format,
                self.types.intl_list_format_object,
                shape,
                storage,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::IntlDateTimeFormat(date_time_format) => self
                .update_embedded_object_storage(
                    date_time_format,
                    self.types.intl_date_time_format_object,
                    shape,
                    storage,
                    |object| &mut object.ordinary,
                ),
            ObjectReceiver::IntlNumberFormat(number_format) => self.update_embedded_object_storage(
                number_format,
                self.types.intl_number_format_object,
                shape,
                storage,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::Number(number) => self.heap.with_running_scope(|scope| {
                let number = scope.root(number).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let number = no_gc
                        .borrow_mut(number, self.types.number_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    number.ordinary.shape = shape;
                    number.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(number, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::BigInt(bigint) => self.heap.with_running_scope(|scope| {
                let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let bigint = no_gc
                        .borrow_mut(bigint, self.types.bigint_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    bigint.ordinary.shape = shape;
                    bigint.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(bigint, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Boolean(boolean) => self.heap.with_running_scope(|scope| {
                let boolean = scope.root(boolean).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let boolean = no_gc
                        .borrow_mut(boolean, self.types.boolean_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    boolean.ordinary.shape = shape;
                    boolean.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(boolean, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::String(string) => self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let string = no_gc
                        .borrow_mut(string, self.types.string_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    string.ordinary.shape = shape;
                    string.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(string, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Symbol(symbol) => self.heap.with_running_scope(|scope| {
                let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let symbol = no_gc
                        .borrow_mut(symbol, self.types.symbol_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    symbol.ordinary.shape = shape;
                    symbol.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(symbol, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::RegExp(regexp) => self.heap.with_running_scope(|scope| {
                let regexp = scope.root(regexp).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let regexp = no_gc
                        .borrow_mut(regexp, self.types.regexp_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    regexp.ordinary.shape = shape;
                    regexp.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(regexp, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::RegExpStringIterator(iterator) => {
                self.heap.with_running_scope(|scope| {
                    let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                    let storage_local = storage
                        .map(|storage| scope.root(storage))
                        .transpose()
                        .map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let iterator = no_gc
                            .borrow_mut(iterator, self.types.regexp_string_iterator)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        iterator.ordinary.shape = shape;
                        iterator.ordinary.storage = storage;
                        Ok::<(), ExecutionError>(())
                    })?;
                    if let Some(storage) = storage_local {
                        scope
                            .write_barrier(iterator, storage)
                            .map_err(ExecutionError::HeapReference)?;
                    }
                    Ok(())
                })
            }
            ObjectReceiver::Promise(promise) => self.heap.with_running_scope(|scope| {
                let promise = scope.root(promise).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let promise = no_gc
                        .borrow_mut(promise, self.types.promise_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    promise.ordinary.shape = shape;
                    promise.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(promise, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Map(map) => self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let map = no_gc
                        .borrow_mut(map, self.types.map_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    map.ordinary.shape = shape;
                    map.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(map, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::Set(set) => self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let set = no_gc
                        .borrow_mut(set, self.types.set_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    set.ordinary.shape = shape;
                    set.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(set, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::WeakMap(map) => self.heap.with_running_scope(|scope| {
                let map = scope.root(map).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let map = no_gc
                        .borrow_mut(map, self.types.weak_map_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    map.ordinary.shape = shape;
                    map.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(map, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::WeakSet(set) => self.heap.with_running_scope(|scope| {
                let set = scope.root(set).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let set = no_gc
                        .borrow_mut(set, self.types.weak_set_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    set.ordinary.shape = shape;
                    set.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(set, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::WeakRef(reference) => self.heap.with_running_scope(|scope| {
                let reference = scope.root(reference).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let reference = no_gc
                        .borrow_mut(reference, self.types.weak_ref_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    reference.ordinary.shape = shape;
                    reference.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(reference, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::FinalizationRegistry(registry) => {
                self.heap.with_running_scope(|scope| {
                    let registry = scope.root(registry).map_err(ExecutionError::Root)?;
                    let storage_local = storage
                        .map(|storage| scope.root(storage))
                        .transpose()
                        .map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let registry = no_gc
                            .borrow_mut(registry, self.types.finalization_registry_object)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        registry.ordinary.shape = shape;
                        registry.ordinary.storage = storage;
                        Ok::<(), ExecutionError>(())
                    })?;
                    if let Some(storage) = storage_local {
                        scope
                            .write_barrier(registry, storage)
                            .map_err(ExecutionError::HeapReference)?;
                    }
                    Ok(())
                })
            }
            ObjectReceiver::ArrayIterator(iterator) => self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let iterator = no_gc
                        .borrow_mut(iterator, self.types.array_iterator)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    iterator.ordinary.shape = shape;
                    iterator.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(iterator, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
            ObjectReceiver::AsyncFromSyncIterator(iterator) => {
                self.heap.with_running_scope(|scope| {
                    let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                    let storage_local = storage
                        .map(|storage| scope.root(storage))
                        .transpose()
                        .map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let iterator = no_gc
                            .borrow_mut(iterator, self.types.async_from_sync_iterator)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        iterator.ordinary.shape = shape;
                        iterator.ordinary.storage = storage;
                        Ok::<(), ExecutionError>(())
                    })?;
                    if let Some(storage) = storage_local {
                        scope
                            .write_barrier(iterator, storage)
                            .map_err(ExecutionError::HeapReference)?;
                    }
                    Ok(())
                })
            }
            ObjectReceiver::IteratorHelper(iterator) => self.update_embedded_object_storage(
                iterator,
                self.types.iterator_helper,
                shape,
                storage,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::WrapForValidIterator(iterator) => self.update_embedded_object_storage(
                iterator,
                self.types.wrap_for_valid_iterator,
                shape,
                storage,
                |object| &mut object.ordinary,
            ),
            ObjectReceiver::CollectionIterator(iterator) => self.heap.with_running_scope(|scope| {
                let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
                let storage_local = storage
                    .map(|storage| scope.root(storage))
                    .transpose()
                    .map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let iterator = no_gc
                        .borrow_mut(iterator, self.types.collection_iterator)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    iterator.ordinary.shape = shape;
                    iterator.ordinary.storage = storage;
                    Ok::<(), ExecutionError>(())
                })?;
                if let Some(storage) = storage_local {
                    scope
                        .write_barrier(iterator, storage)
                        .map_err(ExecutionError::HeapReference)?;
                }
                Ok(())
            }),
        }
    }

    /// Mutates an embedded ordinary prototype and publishes its generational Value edge.
    fn set_embedded_object_prototype<T: Trace + 'static>(
        &mut self,
        receiver: GcRef<T>,
        receiver_type: GcType<T>,
        prototype: Value,
        ordinary: fn(&mut T) -> &mut OrdinaryObject,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let receiver = scope.root(receiver).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                ordinary(
                    no_gc
                        .borrow_mut(receiver, receiver_type)
                        .map_err(ExecutionError::NoGcBorrow)?,
                )
                .prototype = prototype;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(receiver, prototype)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Mutates the extensibility bit shared by a dedicated object payload.
    fn set_embedded_object_extensible<T: Trace + 'static>(
        &mut self,
        receiver: GcRef<T>,
        receiver_type: GcType<T>,
        extensible: bool,
        ordinary: fn(&mut T) -> &mut OrdinaryObject,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let receiver = scope.root(receiver).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                ordinary(
                    no_gc
                        .borrow_mut(receiver, receiver_type)
                        .map_err(ExecutionError::NoGcBorrow)?,
                )
                .extensible = extensible;
                Ok(())
            })
        })
    }

    /// Switches the immutable shape id stored in a dedicated object payload.
    fn set_embedded_object_shape<T: Trace + 'static>(
        &mut self,
        receiver: GcRef<T>,
        receiver_type: GcType<T>,
        shape: ShapeId,
        ordinary: fn(&mut T) -> &mut OrdinaryObject,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let receiver = scope.root(receiver).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                ordinary(
                    no_gc
                        .borrow_mut(receiver, receiver_type)
                        .map_err(ExecutionError::NoGcBorrow)?,
                )
                .shape = shape;
                Ok(())
            })
        })
    }

    /// Updates an embedded ordinary payload and publishes its generational storage edge.
    fn update_embedded_object_storage<T: Trace + 'static>(
        &mut self,
        receiver: GcRef<T>,
        receiver_type: GcType<T>,
        shape: ShapeId,
        storage: Option<GcRef<PropertyStorage>>,
        ordinary: fn(&mut T) -> &mut OrdinaryObject,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let receiver = scope.root(receiver).map_err(ExecutionError::Root)?;
            let storage_local = storage
                .map(|storage| scope.root(storage))
                .transpose()
                .map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let receiver = no_gc
                    .borrow_mut(receiver, receiver_type)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let object = ordinary(receiver);
                object.shape = shape;
                object.storage = storage;
                Ok::<(), ExecutionError>(())
            })?;
            if let Some(storage) = storage_local {
                scope
                    .write_barrier(receiver, storage)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    #[inline(always)]
    pub(crate) fn is_object_value(&self, value: Value) -> bool {
        let Some(raw) = value.as_heap_ref() else {
            return false;
        };
        self.heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.arguments_object)
                .is_ok()
            || self.heap.checked_reference(raw, self.types.array).is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.array_buffer_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.data_view_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.typed_array_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.signal_state)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.signal_computed)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.signal_watcher)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.generator_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.date_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.intl_collator_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.intl_locale_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.intl_list_format_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.intl_date_time_format_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.intl_number_format_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.number_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.bigint_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.boolean_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.string_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.symbol_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.regexp_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.regexp_string_iterator)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.promise_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.map_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.set_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.weak_map_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.weak_set_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.weak_ref_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.finalization_registry_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.function)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.error_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.proxy_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.array_iterator)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.collection_iterator)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.iterator_helper)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.wrap_for_valid_iterator)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.module_namespace_object)
                .is_ok()
    }
}

/// Builds compact storage and reindexes live Symbol edges to their new physical slots.
fn compact_property_storage(
    retained: &[RetainedProperty],
) -> Result<CompactPropertyStorage, ExecutionError> {
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(retained.len())
        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
    let symbol_count = retained
        .iter()
        .filter(|property| property.key.symbol_identity().is_some())
        .count();
    let mut symbol_keys = Vec::new();
    symbol_keys
        .try_reserve_exact(symbol_count)
        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
    for (slot, property) in retained.iter().enumerate() {
        slots.push(property.value);
        if let Some(symbol) = property.key.symbol_identity() {
            symbol_keys.push(SymbolPropertyKey::new(
                u32::try_from(slot).map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?,
                symbol,
                if property.value.as_immediate() == Some(Immediate::Hole) {
                    Value::from_immediate(Immediate::Hole)
                } else {
                    symbol.value()
                },
            ));
        }
    }
    Ok(CompactPropertyStorage {
        slots: slots.into_boxed_slice(),
        symbol_keys: symbol_keys.into_boxed_slice(),
    })
}
