//! Fixed ArrayBuffer-backed DataView construction and numeric access.

use super::super::*;
use crate::object::{ArrayBufferData, DataViewObject};
use crate::runtime::callable::DataViewElement;

#[derive(Clone, Copy)]
struct DataViewSnapshot {
    buffer: Value,
    byte_offset: usize,
    byte_length: usize,
}

impl Isolate {
    /// Constructs a fixed-length DataView after validating its branded ArrayBuffer.
    pub(crate) fn create_data_view_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        if site.new_target.as_immediate() == Some(Immediate::Undefined) {
            return Err(ExecutionError::NonConstructor(site.this_value));
        }
        let buffer = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let raw = buffer
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(buffer))?;
        self.heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(buffer))?;
        let offset_value = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let offset = self.ecma_to_index(offset_value)?;
        let data = self.data_view_backing(buffer)?;
        let buffer_length = self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map(|data| data.byte_length)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if offset > buffer_length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let byte_length = if let Some(value) = self.call_argument(site, 2)? {
            if value.as_immediate() == Some(Immediate::Undefined) {
                buffer_length - offset
            } else {
                self.ecma_to_index(value)?
            }
        } else {
            buffer_length - offset
        };
        if byte_length > buffer_length - offset {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let byte_offset = u32::try_from(offset).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let byte_length =
            u32::try_from(byte_length).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let prototype = self.data_view_prototype_for_new_target(site.new_target)?;
        self.data_view_backing(buffer)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.data_view_object,
                0,
                0,
                DataViewObject {
                    buffer,
                    byte_offset,
                    byte_length,
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
            .map(|view| Value::from_heap_ref(view.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns one of the three branded DataView accessor values.
    pub(crate) fn data_view_getter(
        &mut self,
        receiver: Value,
        getter: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.data_view_snapshot(receiver)?;
        if getter != NativeFunction::DataViewBuffer {
            self.data_view_backing(snapshot.buffer)?;
        }
        Ok(match getter {
            NativeFunction::DataViewBuffer => snapshot.buffer,
            NativeFunction::DataViewByteLength => Value::from_f64(snapshot.byte_length as f64),
            NativeFunction::DataViewByteOffset => Value::from_f64(snapshot.byte_offset as f64),
            _ => return Err(ExecutionError::MissingNativeContinuation),
        })
    }

    /// Reads a Number-backed element using explicit endian byte assembly.
    pub(crate) fn data_view_get(
        &mut self,
        site: &CallSite,
        element: DataViewElement,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.data_view_snapshot(site.this_value)?;
        let offset_value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let offset = self.ecma_to_index(offset_value)?;
        let endian_value = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let little_endian = self.is_truthy_value(endian_value)?;
        let bytes = self.data_view_read_bytes(snapshot, offset, element.byte_width())?;
        Ok(data_view_decode(element, bytes, little_endian))
    }

    /// Converts and writes a Number-backed element after all observable conversions finish.
    pub(crate) fn data_view_set(
        &mut self,
        site: &CallSite,
        element: DataViewElement,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.data_view_snapshot(site.this_value)?;
        let offset_value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let offset = self.ecma_to_index(offset_value)?;
        let input = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let number = numeric_value(self.convert_to_number(input)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(input))?;
        let endian_value = self
            .call_argument(site, 2)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let little_endian = self.is_truthy_value(endian_value)?;
        let encoded = data_view_encode(element, number, little_endian);
        self.data_view_write_bytes(snapshot, offset, element.byte_width(), encoded)?;
        Ok(Value::from_immediate(Immediate::Undefined))
    }

    /// Selects the new-target prototype with the current realm as fallback.
    fn data_view_prototype_for_new_target(
        &mut self,
        new_target: Value,
    ) -> Result<Value, ExecutionError> {
        let fallback = self
            .realm
            .data_view_prototype
            .expect("DataView prototype initializes before construction");
        let prototype_atom = self.intern_intrinsic_name(b"prototype")?;
        let prototype = self
            .get_data_property(new_target, prototype_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        Ok(if self.is_object_value(prototype) {
            prototype
        } else {
            fallback
        })
    }

    /// Snapshots immutable view metadata without retaining a no-GC borrow.
    fn data_view_snapshot(&mut self, value: Value) -> Result<DataViewSnapshot, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let view = self
            .heap
            .checked_reference(raw, self.types.data_view_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let view = scope.root(view).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(view, self.types.data_view_object)
                    .map(|view| DataViewSnapshot {
                        buffer: view.buffer,
                        byte_offset: view.byte_offset as usize,
                        byte_length: view.byte_length as usize,
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Copies at most eight checked bytes from the backing store into a stack word.
    fn data_view_read_bytes(
        &mut self,
        view: DataViewSnapshot,
        offset: usize,
        width: usize,
    ) -> Result<[u8; 8], ExecutionError> {
        let data = self.data_view_backing(view.buffer)?;
        let range = data_view_range(view, offset, width)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut output = [0_u8; 8];
                output[..width].copy_from_slice(&data.bytes[range]);
                Ok(output)
            })
        })
    }

    /// Writes at most eight checked bytes without exposing the backing allocation.
    fn data_view_write_bytes(
        &mut self,
        view: DataViewSnapshot,
        offset: usize,
        width: usize,
        bytes: [u8; 8],
    ) -> Result<(), ExecutionError> {
        let data = self.data_view_backing(view.buffer)?;
        let range = data_view_range(view, offset, width)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .bytes[range]
                    .copy_from_slice(&bytes[..width]);
                Ok(())
            })
        })
    }

    /// Resolves the view's ArrayBuffer edge to its current non-detached backing.
    fn data_view_backing(
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
                    .ok_or(ExecutionError::DetachedArrayBuffer)
            })
        })
    }
}

#[inline(always)]
fn data_view_range(
    view: DataViewSnapshot,
    offset: usize,
    width: usize,
) -> Result<core::ops::Range<usize>, ExecutionError> {
    let end = offset
        .checked_add(width)
        .filter(|end| *end <= view.byte_length)
        .ok_or(ExecutionError::InvalidArrayLength)?;
    let start = view
        .byte_offset
        .checked_add(offset)
        .ok_or(ExecutionError::InvalidArrayLength)?;
    let absolute_end = start
        .checked_add(end - offset)
        .ok_or(ExecutionError::InvalidArrayLength)?;
    Ok(start..absolute_end)
}

#[inline]
/// Decodes a stack-local byte word without native-endian or unaligned loads.
pub(super) fn data_view_decode(element: DataViewElement, bytes: [u8; 8], little: bool) -> Value {
    let order2 = |input: [u8; 2]| {
        if little {
            u16::from_le_bytes(input)
        } else {
            u16::from_be_bytes(input)
        }
    };
    let order4 = |input: [u8; 4]| {
        if little {
            u32::from_le_bytes(input)
        } else {
            u32::from_be_bytes(input)
        }
    };
    let order8 = |input: [u8; 8]| {
        if little {
            u64::from_le_bytes(input)
        } else {
            u64::from_be_bytes(input)
        }
    };
    match element {
        DataViewElement::Int8 => Value::from_i32(i8::from_ne_bytes([bytes[0]]) as i32),
        DataViewElement::Uint8 => Value::from_i32(bytes[0] as i32),
        DataViewElement::Int16 => Value::from_i32(order2([bytes[0], bytes[1]]) as i16 as i32),
        DataViewElement::Uint16 => Value::from_i32(order2([bytes[0], bytes[1]]) as i32),
        DataViewElement::Int32 => {
            Value::from_i32(order4([bytes[0], bytes[1], bytes[2], bytes[3]]) as i32)
        }
        DataViewElement::Uint32 => {
            Value::from_f64(order4([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64)
        }
        DataViewElement::Float32 => {
            Value::from_f64(f32::from_bits(order4([bytes[0], bytes[1], bytes[2], bytes[3]])) as f64)
        }
        DataViewElement::Float64 => Value::from_f64(f64::from_bits(order8(bytes))),
    }
}

#[inline]
/// Encodes ECMAScript Number conversion results into a stack-local byte word.
pub(super) fn data_view_encode(element: DataViewElement, number: f64, little: bool) -> [u8; 8] {
    let mut output = [0_u8; 8];
    let bits = match element {
        DataViewElement::Float32 => (number as f32).to_bits() as u64,
        DataViewElement::Float64 => number.to_bits(),
        _ => to_uint32(number) as u64,
    };
    let width = element.byte_width();
    let ordered = if little {
        bits.to_le_bytes()
    } else {
        bits.to_be_bytes()
    };
    if little {
        output[..width].copy_from_slice(&ordered[..width]);
    } else {
        output[..width].copy_from_slice(&ordered[8 - width..]);
    }
    output
}

#[inline(always)]
fn to_uint32(number: f64) -> u32 {
    if !number.is_finite() || number == 0.0 {
        0
    } else {
        number.trunc().rem_euclid(4_294_967_296.0) as u32
    }
}
