//! Stable default-comparator sorting for fixed TypedArray backing stores.

use core::cmp::Ordering;

use super::*;

#[derive(Clone, Copy)]
struct ElementBits([u8; 8]);

impl Isolate {
    /// Sorts a fixed TypedArray with the spec default comparator and one checked writeback.
    pub(crate) fn begin_typed_array_sort(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let compare = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if compare.as_immediate() != Some(Immediate::Undefined) {
            if !self.is_callable_value(compare)? {
                return Err(ExecutionError::NonCallable(compare));
            }
            return self.begin_typed_array_callable_sort(site, compare);
        }
        let snapshot = self.typed_array_snapshot(receiver)?;
        let data = self.typed_array_backing(snapshot.buffer)?;
        let width = snapshot.kind.byte_width();
        let byte_length = snapshot
            .length
            .checked_mul(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = snapshot
            .byte_offset
            .checked_add(byte_length)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let mut elements = Vec::new();
        elements
            .try_reserve_exact(snapshot.length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if end > data.byte_length || end > data.bytes.len() {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                for bytes in data.bytes[snapshot.byte_offset..end].chunks_exact(width) {
                    let mut bits = [0; 8];
                    bits[..width].copy_from_slice(bytes);
                    elements.push(ElementBits(bits));
                }
                Ok(())
            })
        })?;
        elements.sort_by(|left, right| compare_typed_array_bits(snapshot.kind, left, right));
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                for (target, element) in data.bytes[snapshot.byte_offset..end]
                    .chunks_exact_mut(width)
                    .zip(elements)
                {
                    target.copy_from_slice(&element.0[..width]);
                }
                Ok(())
            })
        })?;
        self.write(site.caller_base, site.destination, receiver)
    }
}

/// Provides the total ordering required by TypedArray's default numeric comparator.
fn compare_typed_array_bits(
    kind: TypedArrayKind,
    left: &ElementBits,
    right: &ElementBits,
) -> Ordering {
    match kind {
        TypedArrayKind::Int8 => (left.0[0] as i8).cmp(&(right.0[0] as i8)),
        TypedArrayKind::Uint8 | TypedArrayKind::Uint8Clamped => left.0[0].cmp(&right.0[0]),
        TypedArrayKind::Int16 => i16::from_le_bytes([left.0[0], left.0[1]])
            .cmp(&i16::from_le_bytes([right.0[0], right.0[1]])),
        TypedArrayKind::Uint16 => u16::from_le_bytes([left.0[0], left.0[1]])
            .cmp(&u16::from_le_bytes([right.0[0], right.0[1]])),
        TypedArrayKind::Int32 => {
            i32::from_le_bytes(first_four(left)).cmp(&i32::from_le_bytes(first_four(right)))
        }
        TypedArrayKind::Uint32 => {
            u32::from_le_bytes(first_four(left)).cmp(&u32::from_le_bytes(first_four(right)))
        }
        TypedArrayKind::Float32 => compare_float(
            f32::from_le_bytes(first_four(left)) as f64,
            f32::from_le_bytes(first_four(right)) as f64,
        ),
        TypedArrayKind::Float64 => {
            compare_float(f64::from_le_bytes(left.0), f64::from_le_bytes(right.0))
        }
        TypedArrayKind::BigInt64 => i64::from_le_bytes(left.0).cmp(&i64::from_le_bytes(right.0)),
        TypedArrayKind::BigUint64 => u64::from_le_bytes(left.0).cmp(&u64::from_le_bytes(right.0)),
    }
}

#[inline(always)]
fn first_four(bits: &ElementBits) -> [u8; 4] {
    [bits.0[0], bits.0[1], bits.0[2], bits.0[3]]
}

/// Orders NaN last and distinguishes negative zero before positive zero.
fn compare_float(left: f64, right: f64) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) if left == 0.0 && right == 0.0 => {
            right.is_sign_negative().cmp(&left.is_sign_negative())
        }
        (false, false) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
    }
}
