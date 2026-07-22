//! ECMAScript iterator payloads and Array iterator state transitions.

use tachyon_gc::{AllocationSpace, Trace, Tracer};
use tachyon_value::Value;

use crate::{
    ExecutionError, Immediate, Isolate, ShapeId, VmRoots,
    array::MAX_SAFE_INTEGER,
    conversion::{numeric_value, safe_integer_value},
    object::{OrdinaryObject, PropertyKey},
    property::PropertyRead,
};

/// The result projection selected when an Array iterator is created.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "key and pair modes are reserved for iterator consumers"
)]
#[repr(u8)]
pub(crate) enum ArrayIterationKind {
    Key,
    Value,
    KeyAndValue,
}

/// Result of one iterator step before an observable getter is entered.
pub(crate) enum ArrayIteratorNextAction {
    Done(Value),
    Get {
        iterator: Value,
        receiver: Value,
        callee: Value,
        mode: crate::PropertyCallbackMode,
    },
}

/// GC-managed Array iterator internal slots plus the ordinary object header.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ArrayIteratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) iterated_object: Option<Value>,
    pub(crate) next_index: u64,
    pub(crate) kind: ArrayIterationKind,
}

/// The result projection selected by a Map or Set iterator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum CollectionIterationKind {
    Key,
    Value,
    KeyAndValue,
}

/// GC-managed Map/Set iterator retaining its exotic identity rather than one replaceable backing.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct CollectionIteratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) collection: Option<Value>,
    pub(crate) next_index: u32,
    pub(crate) kind: CollectionIterationKind,
}

impl Trace for CollectionIteratorObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.collection.trace(tracer);
    }
}

impl Trace for ArrayIteratorObject {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.iterated_object.trace(tracer);
    }
}

struct ArrayIteratorAllocationRoots<'a> {
    vm: VmRoots<'a>,
    iterated_object: Value,
    prototype: Value,
}

struct CollectionIteratorAllocationRoots<'a> {
    vm: VmRoots<'a>,
    collection: Value,
    prototype: Value,
}

struct IteratorResultRoots<'a> {
    vm: VmRoots<'a>,
    value: Value,
    prototype: Value,
}

impl Trace for IteratorResultRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.value.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Trace for ArrayIteratorAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.iterated_object.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Trace for CollectionIteratorAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.collection.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Isolate {
    /// Creates a live Map/Set iterator whose cursor remains valid across backing replacement.
    pub(crate) fn create_collection_iterator(
        &mut self,
        collection: Value,
        kind: CollectionIterationKind,
        map: bool,
    ) -> Result<Value, ExecutionError> {
        let prototype = if map {
            self.realm.map_iterator_prototype
        } else {
            self.realm.set_iterator_prototype
        }
        .expect("collection iterator prototype initializes before iterator creation");
        let mut roots = CollectionIteratorAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            collection,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.collection_iterator,
                0,
                0,
                CollectionIteratorObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    collection: Some(roots.collection),
                    next_index: 0,
                    kind,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|iterator| Value::from_heap_ref(iterator.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Advances a live Map/Set iterator, rereading physical storage after every mutation.
    pub(crate) fn collection_iterator_next(
        &mut self,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        let reference = self.collection_iterator_reference(value)?;
        loop {
            let snapshot = self.collection_iterator_snapshot(reference)?;
            let Some(collection) = snapshot.collection else {
                return self
                    .create_iterator_result(Value::from_immediate(Immediate::Undefined), true);
            };
            let storage = if self.is_map_value(collection) {
                self.map_storage(collection)?
            } else if self.is_set_value(collection) {
                self.set_storage(collection)?
            } else {
                return Err(ExecutionError::IncompatibleCollectionReceiver(collection));
            };
            let used = self.collection_used(storage)?;
            if snapshot.next_index >= used {
                self.finish_collection_iterator(reference)?;
                return self
                    .create_iterator_result(Value::from_immediate(Immediate::Undefined), true);
            }
            self.set_collection_iterator_index(reference, snapshot.next_index + 1)?;
            let Some(entry) = self.collection_entry(storage, snapshot.next_index)? else {
                continue;
            };
            let result = match snapshot.kind {
                CollectionIterationKind::Key => entry.key,
                CollectionIterationKind::Value => entry.value,
                CollectionIterationKind::KeyAndValue => {
                    self.create_collection_entry_array(entry.key, entry.value)?
                }
            };
            return self.create_iterator_result(result, false);
        }
    }

    /// Creates an Array iterator with GC-owned internal slots and no observable state properties.
    pub(crate) fn create_array_iterator(
        &mut self,
        iterated_object: Value,
        kind: ArrayIterationKind,
    ) -> Result<Value, ExecutionError> {
        if !self.is_object_value(iterated_object) {
            return Err(ExecutionError::NotObject(iterated_object));
        }
        let prototype = self
            .realm
            .array_iterator_prototype
            .expect("Array iterator prototype initializes before iterator creation");
        let mut roots = ArrayIteratorAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            iterated_object,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.array_iterator,
                0,
                0,
                ArrayIteratorObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    iterated_object: Some(roots.iterated_object),
                    next_index: 0,
                    kind,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|iterator| Value::from_heap_ref(iterator.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Starts `ArrayIterator.prototype.next`, suspending on a live length or element accessor.
    #[allow(
        dead_code,
        reason = "iterator bytecode lowering will call this resumable entry point"
    )]
    pub(crate) fn array_iterator_next_start(
        &mut self,
        iterator_value: Value,
    ) -> Result<ArrayIteratorNextAction, ExecutionError> {
        let reference = self.array_iterator_reference(iterator_value)?;
        let snapshot = self.array_iterator_snapshot(reference)?;
        let Some(iterated_object) = snapshot.iterated_object else {
            return Ok(ArrayIteratorNextAction::Done(self.create_iterator_result(
                Value::from_immediate(Immediate::Undefined),
                true,
            )?));
        };
        let length_key = PropertyKey::Atom(self.length_atom()?);
        match self.resolve_array_iterator_read(iterated_object, length_key)? {
            PropertyRead::Accessor(callee)
                if callee.as_immediate() != Some(Immediate::Undefined) =>
            {
                Ok(ArrayIteratorNextAction::Get {
                    iterator: iterator_value,
                    receiver: iterated_object,
                    callee,
                    mode: crate::PropertyCallbackMode::ArrayIteratorLength,
                })
            }
            PropertyRead::Data(value) => self.array_iterator_after_length(iterator_value, value),
            PropertyRead::Accessor(_) | PropertyRead::Missing => self.array_iterator_after_length(
                iterator_value,
                Value::from_immediate(Immediate::Undefined),
            ),
        }
    }

    /// Resumes a length getter and either completes or enters the current element getter.
    pub(crate) fn array_iterator_resume_length(
        &mut self,
        iterator: Value,
        length: Value,
    ) -> Result<ArrayIteratorNextAction, ExecutionError> {
        self.array_iterator_after_length(iterator, length)
    }

    /// Resumes an element getter and materializes the iterator result record.
    pub(crate) fn array_iterator_resume_element(
        &mut self,
        iterator: Value,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        let reference = self.array_iterator_reference(iterator)?;
        let snapshot = self.array_iterator_snapshot(reference)?;
        self.create_iterator_result(
            if snapshot.kind == ArrayIterationKind::Key {
                safe_integer_value(snapshot.next_index.saturating_sub(1))
            } else {
                value
            },
            false,
        )
    }

    fn array_iterator_after_length(
        &mut self,
        iterator: Value,
        length_value: Value,
    ) -> Result<ArrayIteratorNextAction, ExecutionError> {
        let reference = self.array_iterator_reference(iterator)?;
        let snapshot = self.array_iterator_snapshot(reference)?;
        let Some(iterated_object) = snapshot.iterated_object else {
            return Ok(ArrayIteratorNextAction::Done(self.create_iterator_result(
                Value::from_immediate(Immediate::Undefined),
                true,
            )?));
        };
        let number = self.convert_to_number(length_value)?;
        let number = numeric_value(number)
            .ok_or(ExecutionError::UnsupportedNumberConversion(length_value))?;
        let length = if number.is_nan() || number <= 0.0 {
            0
        } else if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
            MAX_SAFE_INTEGER
        } else {
            number.floor() as u64
        };
        if snapshot.next_index >= length {
            self.finish_array_iterator(reference)?;
            return Ok(ArrayIteratorNextAction::Done(self.create_iterator_result(
                Value::from_immediate(Immediate::Undefined),
                true,
            )?));
        }
        self.set_array_iterator_index(reference, snapshot.next_index + 1)?;
        if snapshot.kind == ArrayIterationKind::Key {
            return Ok(ArrayIteratorNextAction::Done(self.create_iterator_result(
                safe_integer_value(snapshot.next_index),
                false,
            )?));
        }
        let key = PropertyKey::Atom(self.safe_integer_property_atom(snapshot.next_index)?);
        match self.resolve_array_iterator_read(iterated_object, key)? {
            PropertyRead::Data(value) => Ok(ArrayIteratorNextAction::Done(
                self.create_iterator_result(value, false)?,
            )),
            PropertyRead::Accessor(callee)
                if callee.as_immediate() != Some(Immediate::Undefined) =>
            {
                Ok(ArrayIteratorNextAction::Get {
                    iterator,
                    receiver: iterated_object,
                    callee,
                    mode: crate::PropertyCallbackMode::ArrayIteratorElement,
                })
            }
            PropertyRead::Accessor(_) | PropertyRead::Missing => Ok(ArrayIteratorNextAction::Done(
                self.create_iterator_result(Value::from_immediate(Immediate::Undefined), false)?,
            )),
        }
    }

    /// Reads an Array iterator's live array-like source, forwarding transparent Proxy layers.
    fn resolve_array_iterator_read(
        &mut self,
        mut receiver: Value,
        key: PropertyKey,
    ) -> Result<PropertyRead, ExecutionError> {
        while self.is_proxy_value(receiver) {
            let snapshot = self.proxy_snapshot(receiver)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let get_atom = self.intern_intrinsic_name(b"get")?;
            let transparent = match self.resolve_property_read(snapshot.handler, get_atom.into())? {
                PropertyRead::Missing => true,
                PropertyRead::Data(value)
                    if matches!(
                        value.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) =>
                {
                    true
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    true
                }
                PropertyRead::Data(_) | PropertyRead::Accessor(_) => false,
            };
            if !transparent {
                return Err(ExecutionError::NotObject(receiver));
            }
            receiver = snapshot.target;
        }
        self.resolve_property_read(receiver, key)
    }

    fn array_iterator_reference(
        &mut self,
        value: Value,
    ) -> Result<tachyon_gc::GcRef<ArrayIteratorObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        self.heap
            .checked_reference(raw, self.types.array_iterator)
            .map_err(|_| ExecutionError::NotObject(value))
    }

    fn array_iterator_snapshot(
        &mut self,
        reference: tachyon_gc::GcRef<ArrayIteratorObject>,
    ) -> Result<ArrayIteratorObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(iterator, self.types.array_iterator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Mutates only the numeric iterator cursor; no write barrier is required for this field.
    fn set_array_iterator_index(
        &mut self,
        iterator: tachyon_gc::GcRef<ArrayIteratorObject>,
        next_index: u64,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(iterator, self.types.array_iterator)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .next_index = next_index;
                Ok(())
            })
        })
    }

    /// Clears the iterated-object edge when exhaustion makes it unreachable by specification.
    fn finish_array_iterator(
        &mut self,
        iterator: tachyon_gc::GcRef<ArrayIteratorObject>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(iterator, self.types.array_iterator)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .iterated_object = None;
                Ok(())
            })
        })
    }

    fn collection_iterator_reference(
        &mut self,
        value: Value,
    ) -> Result<tachyon_gc::GcRef<CollectionIteratorObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        self.heap
            .checked_reference(raw, self.types.collection_iterator)
            .map_err(|_| ExecutionError::NotObject(value))
    }

    fn collection_iterator_snapshot(
        &mut self,
        reference: tachyon_gc::GcRef<CollectionIteratorObject>,
    ) -> Result<CollectionIteratorObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(iterator, self.types.collection_iterator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_collection_iterator_index(
        &mut self,
        iterator: tachyon_gc::GcRef<CollectionIteratorObject>,
        next_index: u32,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(iterator, self.types.collection_iterator)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .next_index = next_index;
                Ok(())
            })
        })
    }

    fn finish_collection_iterator(
        &mut self,
        iterator: tachyon_gc::GcRef<CollectionIteratorObject>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(iterator, self.types.collection_iterator)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .collection = None;
                Ok(())
            })
        })
    }

    /// Allocates the two-element array required for Map entries and Set key/value pairs.
    fn create_collection_entry_array(
        &mut self,
        key: Value,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before collection iterator entries");
        let array = self.create_array_object_with_prototype(prototype)?;
        let first = self.safe_integer_property_atom(0)?;
        let second = self.safe_integer_property_atom(1)?;
        let length = self.length_atom()?;
        self.set_own_data_property(array, first, key)?;
        self.set_own_data_property(array, second, value)?;
        self.set_own_data_property(array, length, Value::from_i32(2))?;
        Ok(array)
    }

    /// Materializes the spec result record with the mandated writable/enumerable/configurable fields.
    pub(crate) fn create_iterator_result(
        &mut self,
        value: Value,
        done: bool,
    ) -> Result<Value, ExecutionError> {
        let value_atom = self.intern_intrinsic_name(b"value")?;
        let done_atom = self.intern_intrinsic_name(b"done")?;
        let prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before iterator results");
        let mut roots = IteratorResultRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            value,
            prototype,
        };
        let result = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: roots.prototype,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)?;
        let rooted_value = roots.value;
        self.set_own_data_property(result, value_atom, rooted_value)?;
        self.set_own_data_property(
            result,
            done_atom,
            Value::from_immediate(if done {
                Immediate::True
            } else {
                Immediate::False
            }),
        )?;
        Ok(result)
    }
}
