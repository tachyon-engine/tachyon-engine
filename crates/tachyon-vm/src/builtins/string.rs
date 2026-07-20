//! Primitive String prototype native methods.

use super::super::*;

impl Isolate {
    /// Implements String.prototype.charAt over the engine's UTF-16 code-unit representation.
    pub(crate) fn string_char_at(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let Some(unit) = self.string_code_unit_at(site)? else {
            return self.allocate_runtime_string(
                JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
            );
        };
        self.allocate_runtime_string(
            JsString::try_from_utf16(&[unit]).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements String.prototype.charCodeAt, returning NaN when the position is outside the input.
    pub(crate) fn string_char_code_at(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        Ok(self.string_code_unit_at(site)?.map_or_else(
            || Value::from_f64(f64::NAN),
            |unit| Value::from_i32(i32::from(unit)),
        ))
    }

    /// Implements String.prototype.slice with relative UTF-16 code-unit positions.
    pub(crate) fn string_slice(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(site.this_value)?;
        let length = units.len();
        let start_value = self.call_argument(site, 0)?;
        let end_value = self.call_argument(site, 1)?;
        let start = self.string_relative_index(start_value, length, 0)?;
        let end = self.string_relative_index(end_value, length, length)?;
        let (start, end) = if end < start {
            (start, start)
        } else {
            (start, end)
        };
        let slice = &units[start..end];
        self.allocate_runtime_string(
            JsString::try_from_utf16(slice).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements String.prototype.substring with clamped, source-order-independent positions.
    pub(crate) fn string_substring(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(site.this_value)?;
        let length = units.len();
        let start_value = self.call_argument(site, 0)?;
        let end_value = self.call_argument(site, 1)?;
        let start = self.string_substring_index(start_value, length, 0)?;
        let end = self.string_substring_index(end_value, length, length)?;
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units[start..end])
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Reads one primitive receiver unit after the currently supported ToIntegerOrInfinity conversion.
    fn string_code_unit_at(&mut self, site: &CallSite) -> Result<Option<u16>, ExecutionError> {
        let receiver = site.this_value;
        if !self.is_string_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let position = self
            .call_argument(site, 0)?
            .map(|value| self.convert_to_number(value))
            .transpose()?
            .and_then(numeric_value)
            .unwrap_or(0.0);
        let position = if position.is_nan() || position == 0.0 {
            0.0
        } else {
            position.trunc()
        };
        if !(0.0..=(usize::MAX as f64)).contains(&position) {
            return Ok(None);
        }
        let index = position as usize;
        let raw = receiver.as_heap_ref().expect("primitive String is managed");
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(|string| string.code_unit_at(index))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Collects one primitive String's exact code units before an allocating builtin result is made.
    fn string_receiver_units(&mut self, receiver: Value) -> Result<Vec<u16>, ExecutionError> {
        if !self.is_string_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let raw = receiver.as_heap_ref().expect("primitive String is managed");
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut units = Vec::new();
                units
                    .try_reserve_exact(string.len())
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                for index in 0..string.len() {
                    units.push(string.code_unit_at(index).expect("bounded code-unit index"));
                }
                Ok(units)
            })
        })
    }

    /// Applies ToIntegerOrInfinity and clamps the result using String.prototype.slice rules.
    fn string_relative_index(
        &mut self,
        value: Option<Value>,
        length: usize,
        default: usize,
    ) -> Result<usize, ExecutionError> {
        let Some(value) = value.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(default);
        };
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let integer = if number.is_nan() || number == 0.0 {
            0.0
        } else {
            number.trunc()
        };
        if integer.is_infinite() {
            return Ok(if integer.is_sign_negative() {
                0
            } else {
                length
            });
        }
        if integer >= 0.0 {
            return Ok((integer as usize).min(length));
        }
        Ok(length.saturating_sub((-integer) as usize))
    }

    /// Applies String.prototype.substring's ToIntegerOrInfinity clamping rules.
    fn string_substring_index(
        &mut self,
        value: Option<Value>,
        length: usize,
        default: usize,
    ) -> Result<usize, ExecutionError> {
        let Some(value) = value.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(default);
        };
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let integer = if number.is_nan() || number == 0.0 {
            0.0
        } else {
            number.trunc()
        };
        if integer <= 0.0 {
            return Ok(0);
        }
        if integer.is_infinite() {
            return Ok(length);
        }
        Ok((integer as usize).min(length))
    }
}
