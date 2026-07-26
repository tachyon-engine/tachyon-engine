//! Fixed Number `%TypedArray.prototype.reverse%` implementation.

use super::*;

impl Isolate {
    /// Validates one fixed view and reverses its raw element blocks without conversion.
    pub(crate) fn begin_typed_array_reverse(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
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
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if end > data.byte_length || end > data.bytes.len() {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                let bytes = &mut data.bytes[snapshot.byte_offset..end];
                match width {
                    1 => reverse_typed_array_elements::<1>(bytes, snapshot.length),
                    2 => reverse_typed_array_elements::<2>(bytes, snapshot.length),
                    4 => reverse_typed_array_elements::<4>(bytes, snapshot.length),
                    8 => reverse_typed_array_elements::<8>(bytes, snapshot.length),
                    _ => return Err(ExecutionError::InvalidArrayLength),
                }
                Ok(())
            })
        })?;
        self.write(site.caller_base, site.destination, receiver)
    }
}

/// Swaps disjoint fixed-width blocks so float payload bits remain untouched.
#[inline(always)]
fn reverse_typed_array_elements<const WIDTH: usize>(bytes: &mut [u8], length: usize) {
    for lower in 0..length / 2 {
        let lower_start = lower * WIDTH;
        let upper_start = (length - lower - 1) * WIDTH;
        let (lower_side, upper_side) = bytes.split_at_mut(upper_start);
        lower_side[lower_start..lower_start + WIDTH].swap_with_slice(&mut upper_side[..WIDTH]);
    }
}
