//! Fixed ArrayBuffer-backed DataView construction and numeric access.

mod float16;

use self::float16::{decode_float16, encode_float16};
use super::super::*;
use super::array_buffer::BufferBacking;
use crate::object::{DataViewObject, ViewLengthMode};
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
        let buffer_length = self.buffer_backing_byte_length(&data)?;
        if offset > buffer_length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let explicit_length = if let Some(value) = self.call_argument(site, 2)? {
            if value.as_immediate() == Some(Immediate::Undefined) {
                None
            } else {
                Some(self.ecma_to_index(value)?)
            }
        } else {
            None
        };
        let byte_length = explicit_length.unwrap_or(buffer_length - offset);
        if byte_length > buffer_length - offset {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let byte_offset = u32::try_from(offset).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let tracking =
            explicit_length.is_none() && self.data_view_array_buffer_is_resizable(buffer)?;
        let byte_length =
            u32::try_from(byte_length).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let prototype = self.data_view_prototype_for_new_target(site.new_target)?;
        self.data_view_backing(buffer)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
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
                    length_mode: if tracking {
                        ViewLengthMode::Tracking
                    } else {
                        ViewLengthMode::Fixed
                    },
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
        if matches!(
            element,
            DataViewElement::BigInt64 | DataViewElement::BigUint64
        ) {
            let bits = if little_endian {
                u64::from_le_bytes(bytes)
            } else {
                u64::from_be_bytes(bytes)
            };
            return self.allocate_bigint_bits(bits, element == DataViewElement::BigInt64);
        }
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
        let endian_value = self
            .call_argument(site, 2)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let little_endian = self.is_truthy_value(endian_value)?;
        let encoded = if matches!(
            element,
            DataViewElement::BigInt64 | DataViewElement::BigUint64
        ) {
            let bigint = self.primitive_to_bigint(input)?;
            let bits = self.bigint_modulo_u64(bigint)?;
            if little_endian {
                bits.to_le_bytes()
            } else {
                bits.to_be_bytes()
            }
        } else {
            let number = numeric_value(self.convert_to_number(input)?)
                .ok_or(ExecutionError::UnsupportedNumberConversion(input))?;
            data_view_encode(element, number, little_endian)
        };
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
        let metadata = self.heap.with_running_scope(|scope| {
            let view = scope.root(view).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(view, self.types.data_view_object)
                    .map(|view| {
                        (
                            view.buffer,
                            view.byte_offset as usize,
                            view.byte_length,
                            view.length_mode,
                        )
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let (buffer, byte_offset, encoded_length, length_mode) = metadata;
        let current = match self.data_view_buffer_length(buffer) {
            Ok(length) => length,
            Err(ExecutionError::DetachedArrayBuffer) => {
                return Ok(DataViewSnapshot {
                    buffer,
                    byte_offset,
                    byte_length: encoded_length as usize,
                });
            }
            Err(error) => return Err(error),
        };
        if length_mode == ViewLengthMode::Tracking {
            let byte_length = current.saturating_sub(byte_offset);
            return Ok(DataViewSnapshot {
                buffer,
                byte_offset: if byte_offset > current {
                    0
                } else {
                    byte_offset
                },
                byte_length: if byte_offset > current {
                    0
                } else {
                    byte_length
                },
            });
        }
        if byte_offset
            .checked_add(encoded_length as usize)
            .is_none_or(|end| end > current)
        {
            return Ok(DataViewSnapshot {
                buffer,
                byte_offset: 0,
                byte_length: 0,
            });
        }
        Ok(DataViewSnapshot {
            buffer,
            byte_offset,
            byte_length: encoded_length as usize,
        })
    }

    /// Checks the branded backing's resizable bit without retaining a borrow.
    fn data_view_array_buffer_is_resizable(
        &mut self,
        buffer: Value,
    ) -> Result<bool, ExecutionError> {
        let backing = self.resolve_buffer_backing(buffer)?;
        self.buffer_backing_is_resizable(&backing)
    }

    /// Reads the current byte length of a branded, attached ArrayBuffer.
    fn data_view_buffer_length(&mut self, buffer: Value) -> Result<usize, ExecutionError> {
        let backing = self.resolve_buffer_backing(buffer)?;
        self.buffer_backing_byte_length(&backing)
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
        let mut output = [0_u8; 8];
        self.read_buffer_backing_bytes(&data, range, &mut output[..width])?;
        Ok(output)
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
        self.write_buffer_backing_bytes(&data, range, &bytes[..width])
    }

    /// Resolves the view's ArrayBuffer edge to its current non-detached backing.
    fn data_view_backing(&mut self, buffer: Value) -> Result<BufferBacking, ExecutionError> {
        self.resolve_buffer_backing(buffer)
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
        DataViewElement::Float16 => Value::from_f64(decode_float16(order2([bytes[0], bytes[1]]))),
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
        DataViewElement::BigInt64 | DataViewElement::BigUint64 => {
            unreachable!("BigInt DataView elements decode through the BigInt path")
        }
    }
}

#[inline]
/// Encodes ECMAScript Number conversion results into a stack-local byte word.
pub(super) fn data_view_encode(element: DataViewElement, number: f64, little: bool) -> [u8; 8] {
    let mut output = [0_u8; 8];
    let bits = match element {
        DataViewElement::Float16 => u64::from(encode_float16(number)),
        DataViewElement::Float32 => (number as f32).to_bits() as u64,
        DataViewElement::Float64 => number.to_bits(),
        DataViewElement::BigInt64 | DataViewElement::BigUint64 => {
            unreachable!("BigInt DataView elements encode through the BigInt path")
        }
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
