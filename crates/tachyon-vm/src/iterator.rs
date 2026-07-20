//! ECMAScript iterator payloads and Array iterator state transitions.

use tachyon_gc::{AllocationSpace, Trace, Tracer};
use tachyon_value::Value;

use crate::{
    ExecutionError, Immediate, Isolate, ShapeId, VmRoots,
    array::MAX_SAFE_INTEGER,
    conversion::{numeric_value, safe_integer_value},
    object::OrdinaryObject,
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

/// GC-managed Array iterator internal slots plus the ordinary object header.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ArrayIteratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) iterated_object: Option<Value>,
    pub(crate) next_index: u64,
    pub(crate) kind: ArrayIterationKind,
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

impl Isolate {
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

    /// Advances one Array iterator and returns the ordinary `{ value, done }` result object.
    pub(crate) fn array_iterator_next(
        &mut self,
        iterator_value: Value,
    ) -> Result<Value, ExecutionError> {
        let raw = iterator_value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(iterator_value))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.array_iterator)
            .map_err(|_| ExecutionError::NotObject(iterator_value))?;
        let snapshot = self.heap.with_running_scope(|scope| {
            let iterator = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(iterator, self.types.array_iterator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let Some(iterated_object) = snapshot.iterated_object else {
            return self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true);
        };
        let length_atom = self.length_atom()?;
        let length_value = self
            .get_data_property(iterated_object, length_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let number = self.convert_to_number(length_value)?;
        let number =
            numeric_value(number).ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        let length = if number.is_nan() || number <= 0.0 {
            0
        } else if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
            MAX_SAFE_INTEGER
        } else {
            number.floor() as u64
        };
        if snapshot.next_index >= length {
            self.finish_array_iterator(reference)?;
            return self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true);
        }
        self.set_array_iterator_index(reference, snapshot.next_index + 1)?;
        let value = match snapshot.kind {
            ArrayIterationKind::Key => safe_integer_value(snapshot.next_index),
            ArrayIterationKind::Value => {
                let key = self.safe_integer_property_atom(snapshot.next_index)?;
                self.get_data_property(iterated_object, key)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined))
            }
            ArrayIterationKind::KeyAndValue => {
                return Err(ExecutionError::UnsupportedPropertyKey(iterator_value));
            }
        };
        self.create_iterator_result(value, false)
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

    /// Materializes the spec result record with the mandated writable/enumerable/configurable fields.
    fn create_iterator_result(
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
