//! Fixed-buffer Number TypedArray construction and integer-indexed element access.

use super::super::*;
use super::data_view::{data_view_decode, data_view_encode};
use crate::conversion::parse_number_code_units;
use crate::object::{ArrayBufferData, TypedArrayKind, TypedArrayObject};
use crate::property::array_index;
use crate::runtime::callable::{DataViewElement, TypedArrayGetter};

#[derive(Clone, Copy)]
pub(crate) struct TypedArraySnapshot {
    pub(crate) buffer: Value,
    pub(crate) byte_offset: usize,
    pub(crate) length: usize,
    pub(crate) kind: TypedArrayKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedArrayIndex {
    NonNumeric,
    Invalid,
    Valid(usize),
}

struct TypedArrayAllocationRoots<'a> {
    vm: VmRoots<'a>,
    buffer: Value,
    prototype: Value,
}

impl Trace for TypedArrayAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.buffer.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Isolate {
    /// Constructs a fixed Number TypedArray from a length or an existing ArrayBuffer.
    pub(crate) fn create_typed_array_from_site(
        &mut self,
        site: &CallSite,
        kind: TypedArrayKind,
    ) -> Result<Value, ExecutionError> {
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let (buffer, byte_offset, length) = if let Some(raw) = argument.as_heap_ref()
            && let Ok(buffer) = self
                .heap
                .checked_reference(raw, self.types.array_buffer_object)
        {
            let buffer_length = self.array_buffer_length_for_view(buffer)?;
            let offset_value = self
                .call_argument(site, 1)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            let byte_offset = self.ecma_to_index(offset_value)?;
            let width = kind.byte_width();
            if byte_offset % width != 0 || byte_offset > buffer_length {
                return Err(ExecutionError::InvalidArrayLength);
            }
            let length_value = self.call_argument(site, 2)?;
            let length = if let Some(value) = length_value
                && value.as_immediate() != Some(Immediate::Undefined)
            {
                let length = self.ecma_to_index(value)?;
                let byte_length = length
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                if byte_length > buffer_length - byte_offset {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                length
            } else {
                let remaining = buffer_length - byte_offset;
                if remaining % width != 0 {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                remaining / width
            };
            (argument, byte_offset, length)
        } else if !self.is_object_value(argument) {
            let length = self.ecma_to_index(argument)?;
            let byte_length = length
                .checked_mul(kind.byte_width())
                .ok_or(ExecutionError::InvalidArrayLength)?;
            let prototype = self
                .realm
                .array_buffer_prototype
                .expect("ArrayBuffer prototype initializes before TypedArray");
            let buffer =
                self.allocate_array_buffer_object(byte_length, byte_length, false, prototype)?;
            (buffer, 0, length)
        } else {
            return Err(ExecutionError::NotObject(argument));
        };
        let byte_offset =
            u32::try_from(byte_offset).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let length = u32::try_from(length).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let prototype = self.typed_array_prototype_for_new_target(site.new_target, kind)?;
        let roots = &mut TypedArrayAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            buffer,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.typed_array_object,
                0,
                0,
                TypedArrayObject {
                    buffer,
                    byte_offset,
                    length,
                    kind,
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
            .map(|array| Value::from_heap_ref(array.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns one shared `%TypedArray.prototype%` accessor result after a brand check.
    pub(crate) fn typed_array_getter(
        &mut self,
        receiver: Value,
        getter: TypedArrayGetter,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.typed_array_snapshot(receiver)?;
        Ok(match getter {
            TypedArrayGetter::Length => safe_integer_value(snapshot.length as u64),
            TypedArrayGetter::Buffer => snapshot.buffer,
            TypedArrayGetter::ByteLength => safe_integer_value(
                snapshot
                    .length
                    .checked_mul(snapshot.kind.byte_width())
                    .ok_or(ExecutionError::InvalidArrayLength)? as u64,
            ),
            TypedArrayGetter::ByteOffset => safe_integer_value(snapshot.byte_offset as u64),
            TypedArrayGetter::ToStringTag => {
                let atom = self.intern_intrinsic_name(snapshot.kind.name().as_bytes())?;
                self.atom_string_value(atom)?
            }
        })
    }

    /// Classifies CanonicalNumericIndexString while keeping common array indices allocation-free.
    pub(crate) fn typed_array_index(
        &mut self,
        key: PropertyKey,
    ) -> Result<TypedArrayIndex, ExecutionError> {
        let Some(atom) = key.atom() else {
            return Ok(TypedArrayIndex::NonNumeric);
        };
        let string = self
            .atoms
            .get(atom)
            .ok_or(ExecutionError::InvalidAtom(atom))?;
        if let Some(index) = array_index(string.as_view()) {
            return Ok(TypedArrayIndex::Valid(index as usize));
        }
        let mut original = Vec::new();
        original
            .try_reserve_exact(string.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let view = string.as_view();
        original.extend(
            (0..view.len()).map(|index| view.code_unit_at(index).expect("index is bounded")),
        );
        if original == [u16::from(b'-'), u16::from(b'0')] {
            return Ok(TypedArrayIndex::Invalid);
        }
        let number = parse_number_code_units(&original);
        let mut canonical = Vec::new();
        self.append_primitive_string_units(Value::from_f64(number), &mut canonical)?;
        if canonical != original {
            return Ok(TypedArrayIndex::NonNumeric);
        }
        if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
            return Ok(TypedArrayIndex::Invalid);
        }
        usize::try_from(number as u64)
            .map(TypedArrayIndex::Valid)
            .map_err(|_| ExecutionError::InvalidArrayLength)
    }

    /// Reads one canonical numeric property, returning None only for non-numeric keys.
    pub(crate) fn typed_array_index_get(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Option<Value>>, ExecutionError> {
        if !self.is_typed_array_value(receiver) {
            return Ok(None);
        }
        let index = match self.typed_array_index(key)? {
            TypedArrayIndex::NonNumeric => return Ok(None),
            TypedArrayIndex::Invalid => return Ok(Some(None)),
            TypedArrayIndex::Valid(index) => index,
        };
        let snapshot = self.typed_array_snapshot(receiver)?;
        if index >= snapshot.length {
            return Ok(Some(None));
        }
        self.typed_array_read_element(snapshot, index)
            .map(Some)
            .map(Some)
    }

    /// Writes one canonical numeric property, returning None only for non-numeric keys.
    pub(crate) fn typed_array_index_set(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<Option<bool>, ExecutionError> {
        if !self.is_typed_array_value(receiver) {
            return Ok(None);
        }
        let index = match self.typed_array_index(key)? {
            TypedArrayIndex::NonNumeric => return Ok(None),
            TypedArrayIndex::Invalid => return Ok(Some(false)),
            TypedArrayIndex::Valid(index) => index,
        };
        let snapshot = self.typed_array_snapshot(receiver)?;
        if index >= snapshot.length {
            return Ok(Some(false));
        }
        let converted = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        self.typed_array_write_element(snapshot, index, converted)?;
        Ok(Some(true))
    }

    #[inline(always)]
    pub(crate) fn is_typed_array_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.typed_array_object)
                .is_ok()
        })
    }

    /// Snapshots immutable fixed-view metadata without retaining a no-GC borrow.
    pub(crate) fn typed_array_snapshot(
        &mut self,
        value: Value,
    ) -> Result<TypedArraySnapshot, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let array = self
            .heap
            .checked_reference(raw, self.types.typed_array_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.typed_array_object)
                    .map(|array| TypedArraySnapshot {
                        buffer: array.buffer,
                        byte_offset: array.byte_offset as usize,
                        length: array.length as usize,
                        kind: array.kind,
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Returns the current byte length while rejecting a detached ArrayBuffer edge.
    fn array_buffer_length_for_view(
        &mut self,
        buffer: GcRef<crate::object::ArrayBufferObject>,
    ) -> Result<usize, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let buffer = scope.root(buffer).map_err(ExecutionError::Root)?;
            let data = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(buffer, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data
                    .ok_or(ExecutionError::InvalidArrayLength)
            })?;
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map(|data| data.byte_length)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Selects a concrete prototype with the current realm constructor kind as fallback.
    fn typed_array_prototype_for_new_target(
        &mut self,
        new_target: Value,
        kind: TypedArrayKind,
    ) -> Result<Value, ExecutionError> {
        let fallback = self.realm.typed_array_prototypes[kind.index()]
            .expect("concrete TypedArray prototype initializes before construction");
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(new_target, prototype_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        Ok(if self.is_object_value(prototype) {
            prototype
        } else {
            fallback
        })
    }

    /// Resolves the ArrayBuffer edge to the currently attached byte backing.
    fn typed_array_backing(
        &mut self,
        buffer: Value,
    ) -> Result<GcRef<ArrayBufferData>, ExecutionError> {
        let raw = buffer
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(buffer))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(buffer))?;
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data
                    .ok_or(ExecutionError::InvalidArrayLength)
            })
        })
    }

    /// Copies one checked element into a stack word and decodes explicit little-endian storage.
    fn typed_array_read_element(
        &mut self,
        array: TypedArraySnapshot,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        let width = array.kind.byte_width();
        let start = array
            .byte_offset
            .checked_add(
                index
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = start
            .checked_add(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let data = self.typed_array_backing(array.buffer)?;
        let bytes = self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut bytes = [0_u8; 8];
                bytes[..width].copy_from_slice(&data.bytes[start..end]);
                Ok(bytes)
            })
        })?;
        Ok(data_view_decode(data_view_kind(array.kind), bytes, true))
    }

    /// Encodes and writes one checked element without publishing a per-index property slot.
    fn typed_array_write_element(
        &mut self,
        array: TypedArraySnapshot,
        index: usize,
        number: f64,
    ) -> Result<(), ExecutionError> {
        let width = array.kind.byte_width();
        let start = array
            .byte_offset
            .checked_add(
                index
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = start
            .checked_add(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let bytes = if array.kind == TypedArrayKind::Uint8Clamped {
            let mut bytes = [0_u8; 8];
            bytes[0] = to_uint8_clamp(number);
            bytes
        } else {
            data_view_encode(data_view_kind(array.kind), number, true)
        };
        let data = self.typed_array_backing(array.buffer)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .bytes[start..end]
                    .copy_from_slice(&bytes[..width]);
                Ok(())
            })
        })
    }
}

#[inline(always)]
fn data_view_kind(kind: TypedArrayKind) -> DataViewElement {
    match kind {
        TypedArrayKind::Int8 => DataViewElement::Int8,
        TypedArrayKind::Uint8 | TypedArrayKind::Uint8Clamped => DataViewElement::Uint8,
        TypedArrayKind::Int16 => DataViewElement::Int16,
        TypedArrayKind::Uint16 => DataViewElement::Uint16,
        TypedArrayKind::Int32 => DataViewElement::Int32,
        TypedArrayKind::Uint32 => DataViewElement::Uint32,
        TypedArrayKind::Float32 => DataViewElement::Float32,
        TypedArrayKind::Float64 => DataViewElement::Float64,
    }
}

/// Implements ToUint8Clamp including ties-to-even at exact half integers.
#[inline]
fn to_uint8_clamp(number: f64) -> u8 {
    if number.is_nan() || number <= 0.0 {
        return 0;
    }
    if number >= 255.0 {
        return 255;
    }
    let floor = number.floor();
    let fraction = number - floor;
    if fraction > 0.5 || (fraction == 0.5 && floor as u8 % 2 == 1) {
        floor as u8 + 1
    } else {
        floor as u8
    }
}
