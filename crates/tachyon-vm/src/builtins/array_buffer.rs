//! Fixed-length ArrayBuffer construction and branded prototype accessors.

use super::super::*;
use crate::object::{ArrayBufferData, ArrayBufferObject};

const MAX_ARRAY_BUFFER_BYTES: usize = u32::MAX as usize;

#[derive(Clone, Copy, Debug)]
struct ArrayBufferDataSnapshot {
    _data: GcRef<ArrayBufferData>,
    byte_length: usize,
    max_byte_length: usize,
    resizable: bool,
}

impl Isolate {
    /// Clears one fixed ArrayBuffer's backing edge; repeated detach is a no-op.
    pub(crate) fn detach_array_buffer(&mut self, value: Value) -> Result<(), ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(object, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data = None;
                Ok(())
            })
        })
    }

    /// Implements the fixed-length `ArrayBuffer` constructor without host allocation hooks.
    pub(crate) fn create_array_buffer_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let length = self.call_argument(site, 0)?.unwrap_or(Value::from_i32(0));
        let length = numeric_value(self.convert_to_number(length)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(length))?;
        if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let length =
            usize::try_from(length as u64).map_err(|_| ExecutionError::InvalidArrayLength)?;
        if length > MAX_ARRAY_BUFFER_BYTES {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let prototype = self.array_buffer_prototype_for_new_target(site.new_target)?;
        self.allocate_array_buffer_object(length, length, false, prototype)
    }

    /// Selects a constructor prototype while preserving the ordinary cross-realm fallback.
    fn array_buffer_prototype_for_new_target(
        &mut self,
        new_target: Value,
    ) -> Result<Value, ExecutionError> {
        let fallback = self
            .realm
            .array_buffer_prototype
            .expect("ArrayBuffer prototype initializes before construction");
        if new_target.as_immediate() == Some(Immediate::Undefined) {
            return Ok(fallback);
        }
        let prototype_atom = self.intern_intrinsic_name(b"prototype")?;
        let prototype = self
            .get_data_property(new_target, prototype_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(prototype) {
            Ok(prototype)
        } else {
            Ok(fallback)
        }
    }

    /// Allocates a byte backing block and an ordinary branded ArrayBuffer object.
    pub(crate) fn allocate_array_buffer_object(
        &mut self,
        byte_length: usize,
        max_byte_length: usize,
        resizable: bool,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(max_byte_length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        bytes.resize(max_byte_length, 0);
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let data = self
            .heap
            .try_allocate_external_with_gc(
                self.types.array_buffer_data,
                0,
                ArrayBufferData {
                    bytes: bytes.into_boxed_slice(),
                    byte_length,
                    max_byte_length,
                    resizable,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.heap
            .try_allocate_with_gc(
                self.types.array_buffer_object,
                0,
                0,
                ArrayBufferObject {
                    data: Some(data),
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns a branded backing snapshot for a fixed ArrayBuffer operation.
    fn array_buffer_data_snapshot(
        &mut self,
        value: Value,
    ) -> Result<Option<ArrayBufferDataSnapshot>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        let data = self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.array_buffer_object)
                    .map(|object| object.data)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let Some(data) = data else { return Ok(None) };
        let snapshot = self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map(|data| (data.byte_length, data.max_byte_length, data.resizable))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok(Some(ArrayBufferDataSnapshot {
            _data: data,
            byte_length: snapshot.0,
            max_byte_length: snapshot.1,
            resizable: snapshot.2,
        }))
    }

    /// Implements the branded `byteLength`, `maxByteLength`, `resizable`, and `detached` getters.
    pub(crate) fn array_buffer_getter(
        &mut self,
        receiver: Value,
        getter: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let Some(snapshot) = self.array_buffer_data_snapshot(receiver)? else {
            return Ok(match getter {
                NativeFunction::ArrayBufferDetached => Value::from_immediate(Immediate::True),
                NativeFunction::ArrayBufferByteLength
                | NativeFunction::ArrayBufferMaxByteLength => Value::from_i32(0),
                NativeFunction::ArrayBufferResizable => Value::from_immediate(Immediate::False),
                _ => return Err(ExecutionError::MissingNativeContinuation),
            });
        };
        Ok(match getter {
            NativeFunction::ArrayBufferByteLength => Value::from_f64(snapshot.byte_length as f64),
            NativeFunction::ArrayBufferMaxByteLength => {
                Value::from_f64(snapshot.max_byte_length as f64)
            }
            NativeFunction::ArrayBufferResizable => boolean_value(snapshot.resizable),
            NativeFunction::ArrayBufferDetached => Value::from_immediate(Immediate::False),
            _ => return Err(ExecutionError::MissingNativeContinuation),
        })
    }

    /// Implements `ArrayBuffer.isView` for the currently installed view brands.
    pub(crate) fn array_buffer_is_view(&mut self, value: Value) -> Value {
        let is_view = value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.data_view_object)
                .is_ok()
                || self
                    .heap
                    .checked_reference(raw, self.types.typed_array_object)
                    .is_ok()
        });
        boolean_value(is_view)
    }
}
