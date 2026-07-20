//! Primitive String prototype native methods.

use super::super::*;

impl Isolate {
    /// Returns the primitive String receiver required by String.prototype.toString and valueOf.
    pub(crate) fn string_primitive_value(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        if self.is_string_value(receiver) {
            Ok(receiver)
        } else {
            Err(ExecutionError::NotObject(receiver))
        }
    }

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

    /// Implements String.prototype.at with relative UTF-16 code-unit indexing.
    pub(crate) fn string_at(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(site.this_value)?;
        let argument = self.call_argument(site, 0)?;
        let position = self.string_at_position(argument, units.len())?;
        match position.and_then(|index| units.get(index)) {
            Some(unit) => self.allocate_runtime_string(
                JsString::try_from_utf16(&[*unit]).map_err(ExecutionError::PropertyKeyString)?,
            ),
            None => Ok(Value::from_immediate(Immediate::Undefined)),
        }
    }

    /// Reads one Unicode scalar or unpaired UTF-16 unit at the requested code-unit position.
    pub(crate) fn string_code_point_at(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(site.this_value)?;
        let argument = self.call_argument(site, 0)?;
        let position = self.string_at_position(argument, units.len())?;
        let Some(&first) = position.and_then(|index| units.get(index)) else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        let position = position.expect("a present code unit has an index");
        let code_point = if let Some(&second) = units.get(position + 1)
            && (0xd800..=0xdbff).contains(&first)
            && (0xdc00..=0xdfff).contains(&second)
        {
            0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
        } else {
            u32::from(first)
        };
        Ok(Value::from_f64(f64::from(code_point)))
    }

    /// Materializes each argument's ToUint16 value into a single UTF-16 string.
    pub(crate) fn string_from_char_code(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let mut units = Vec::new();
        units
            .try_reserve_exact(site.argument_count as usize)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..site.argument_count {
            let value = self
                .call_argument(site, index)?
                .expect("argument count is bounded");
            let number = numeric_value(self.convert_to_number(value)?)
                .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
            let unit = if !number.is_finite() || number == 0.0 {
                0
            } else {
                number.trunc().rem_euclid(65_536.0) as u16
            };
            units.push(unit);
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Validates Unicode scalar arguments and writes their exact UTF-16 encoding once.
    pub(crate) fn string_from_code_point(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let mut units = Vec::new();
        units
            .try_reserve_exact(site.argument_count as usize)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..site.argument_count {
            let value = self
                .call_argument(site, index)?
                .expect("argument count is bounded");
            let number = numeric_value(self.convert_to_number(value)?)
                .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
            if !number.is_finite()
                || number.fract() != 0.0
                || !(0.0..=0x10_ffff as f64).contains(&number)
            {
                return Err(ExecutionError::InvalidStringLength);
            }
            let code_point = number as u32;
            if let Some(character) = char::from_u32(code_point) {
                let mut encoded = [0; 2];
                units.extend_from_slice(character.encode_utf16(&mut encoded));
            } else {
                return Err(ExecutionError::InvalidStringLength);
            }
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
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

    /// Implements String.prototype.indexOf with an exact UTF-16 code-unit search.
    pub(crate) fn string_index_of(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let haystack = self.string_receiver_units(site.this_value)?;
        let needle = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let mut needle_units = Vec::new();
        self.append_primitive_string_units(needle, &mut needle_units)?;
        let start_value = self.call_argument(site, 1)?;
        let start = self.string_search_start(start_value, haystack.len())?;
        if needle_units.is_empty() {
            return Ok(safe_integer_value(start as u64));
        }
        if needle_units.len() > haystack.len().saturating_sub(start) {
            return Ok(Value::from_i32(-1));
        }
        let last = haystack.len() - needle_units.len();
        for index in start..=last {
            if haystack[index..index + needle_units.len()] == needle_units {
                return Ok(safe_integer_value(index as u64));
            }
        }
        Ok(Value::from_i32(-1))
    }

    /// Implements String.prototype.includes through the same code-unit search as indexOf.
    pub(crate) fn string_includes(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let found = self
            .string_index_of(site)?
            .as_i32()
            .is_some_and(|index| index >= 0);
        Ok(Value::from_immediate(if found {
            Immediate::True
        } else {
            Immediate::False
        }))
    }

    /// Finds the final UTF-16 code-unit occurrence at or before the normalized position.
    pub(crate) fn string_last_index_of(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let haystack = self.string_receiver_units(site.this_value)?;
        let needle = self.string_argument_units(site, 0)?;
        let position_value = self.call_argument(site, 1)?;
        let position = self.string_last_search_start(position_value, haystack.len())?;
        if needle.len() > haystack.len() {
            return Ok(Value::from_i32(-1));
        }
        let last = haystack.len().saturating_sub(needle.len()).min(position);
        for index in (0..=last).rev() {
            if haystack[index..index + needle.len()] == needle {
                return Ok(safe_integer_value(index as u64));
            }
        }
        Ok(Value::from_i32(-1))
    }

    /// Tests whether the UTF-16 needle occurs at the normalized start position.
    pub(crate) fn string_starts_with(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let haystack = self.string_receiver_units(site.this_value)?;
        let needle = self.string_argument_units(site, 0)?;
        let position = self.call_argument(site, 1)?;
        let start = self.string_search_start(position, haystack.len())?;
        Ok(boolean_value(
            needle.len() <= haystack.len().saturating_sub(start)
                && haystack[start..start + needle.len()] == needle,
        ))
    }

    /// Tests whether the UTF-16 needle ends at the normalized end position.
    pub(crate) fn string_ends_with(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let haystack = self.string_receiver_units(site.this_value)?;
        let needle = self.string_argument_units(site, 0)?;
        let position = self.call_argument(site, 1)?;
        let end = self.string_substring_index(position, haystack.len(), haystack.len())?;
        let start = end.saturating_sub(needle.len());
        Ok(boolean_value(
            needle.len() <= end && haystack[start..end] == needle,
        ))
    }

    /// Concatenates every primitive argument after calculating one exact output capacity.
    pub(crate) fn string_concat(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let mut capacity = self.string_value_length(site.this_value)?;
        for index in 0..site.argument_count {
            let argument = self
                .call_argument(site, index)?
                .expect("argument count is bounded");
            capacity = capacity
                .checked_add(self.primitive_string_unit_length(argument)?)
                .filter(|length| *length <= u32::MAX as usize)
                .ok_or(ExecutionError::InvalidStringLength)?;
        }
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        self.append_primitive_string_units(site.this_value, &mut units)?;
        for index in 0..site.argument_count {
            let argument = self
                .call_argument(site, index)?
                .expect("argument count is bounded");
            self.append_primitive_string_units(argument, &mut units)?;
        }
        debug_assert_eq!(units.len(), capacity);
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Repeats the receiver after rejecting negative, infinite, and overlong results.
    pub(crate) fn string_repeat(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(site.this_value)?;
        let count_value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let number = numeric_value(self.convert_to_number(count_value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(count_value))?;
        let count = if number.is_nan() || number == 0.0 {
            0
        } else if !number.is_finite() || number < 0.0 || number > u32::MAX as f64 {
            return Err(ExecutionError::InvalidStringRepeatCount(count_value));
        } else {
            number.trunc() as usize
        };
        let capacity = units
            .len()
            .checked_mul(count)
            .filter(|length| *length <= u32::MAX as usize)
            .ok_or(ExecutionError::InvalidStringLength)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for _ in 0..count {
            output.extend_from_slice(&units);
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Pads from either boundary by repeating and truncating the UTF-16 fill sequence.
    pub(crate) fn string_pad(
        &mut self,
        site: &CallSite,
        pad_end: bool,
    ) -> Result<Value, ExecutionError> {
        let receiver = self.string_receiver_units(site.this_value)?;
        let target_value = self.call_argument(site, 0)?;
        let target = self.string_target_length(target_value)?;
        if target <= receiver.len() {
            return self.allocate_runtime_string(
                JsString::try_from_owned_code_units(receiver)
                    .map_err(ExecutionError::PropertyKeyString)?,
            );
        }
        let fill = match self.call_argument(site, 1)? {
            Some(value) if value.as_immediate() != Some(Immediate::Undefined) => {
                self.primitive_string_units(value)?
            }
            _ => vec![u16::from(b' ')],
        };
        if fill.is_empty() {
            return self.allocate_runtime_string(
                JsString::try_from_owned_code_units(receiver)
                    .map_err(ExecutionError::PropertyKeyString)?,
            );
        }
        if target > u32::MAX as usize {
            return Err(ExecutionError::InvalidStringLength);
        }
        let fill_length = target - receiver.len();
        let mut output = Vec::new();
        output
            .try_reserve_exact(target)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        if !pad_end {
            append_repeated_prefix(&mut output, &fill, fill_length);
        }
        output.extend_from_slice(&receiver);
        if pad_end {
            append_repeated_prefix(&mut output, &fill, fill_length);
        }
        debug_assert_eq!(output.len(), target);
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Trims ECMAScript WhiteSpace and LineTerminator code units from either string boundary.
    pub(crate) fn string_trim(
        &mut self,
        receiver: Value,
        trim_start: bool,
        trim_end: bool,
    ) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(receiver)?;
        let mut start = 0;
        let mut end = units.len();
        if trim_start {
            while start < end && is_ecmascript_trim_unit(units[start]) {
                start += 1;
            }
        }
        if trim_end {
            while end > start && is_ecmascript_trim_unit(units[end - 1]) {
                end -= 1;
            }
        }
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

    /// Converts one argument with String's current primitive conversion substrate.
    fn string_argument_units(
        &mut self,
        site: &CallSite,
        index: u32,
    ) -> Result<Vec<u16>, ExecutionError> {
        let value = self
            .call_argument(site, index)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.primitive_string_units(value)
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

    /// Normalizes String.prototype.indexOf's optional fromIndex without relative negative indexing.
    fn string_search_start(
        &mut self,
        value: Option<Value>,
        length: usize,
    ) -> Result<usize, ExecutionError> {
        let Some(value) = value.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(0);
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

    /// Normalizes the optional lastIndexOf position, whose omitted value starts at the end.
    fn string_last_search_start(
        &mut self,
        value: Option<Value>,
        length: usize,
    ) -> Result<usize, ExecutionError> {
        let Some(value) = value.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(length);
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

    /// Normalizes String.prototype.at and codePointAt positions, retaining an absent result.
    fn string_at_position(
        &mut self,
        value: Option<Value>,
        length: usize,
    ) -> Result<Option<usize>, ExecutionError> {
        let Some(value) = value.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok((length > 0).then_some(0));
        };
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if !number.is_finite() {
            return Ok(None);
        }
        let integer = if number.is_nan() { 0.0 } else { number.trunc() };
        let index = if integer < 0.0 {
            length as f64 + integer
        } else {
            integer
        };
        Ok((0.0..length as f64)
            .contains(&index)
            .then_some(index as usize))
    }

    /// Implements ToLength for pad targets within the engine's representable string limit.
    fn string_target_length(&mut self, value: Option<Value>) -> Result<usize, ExecutionError> {
        let Some(value) = value.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(0);
        };
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if !number.is_finite() || number > u32::MAX as f64 {
            return Err(ExecutionError::InvalidStringLength);
        }
        Ok(number.trunc() as usize)
    }
}

#[inline(always)]
fn boolean_value(value: bool) -> Value {
    Value::from_immediate(if value {
        Immediate::True
    } else {
        Immediate::False
    })
}

/// Extends `output` with exactly `length` code units from a cyclic fill sequence.
fn append_repeated_prefix(output: &mut Vec<u16>, fill: &[u16], length: usize) {
    let full_repeats = length / fill.len();
    for _ in 0..full_repeats {
        output.extend_from_slice(fill);
    }
    output.extend_from_slice(&fill[..length % fill.len()]);
}

#[inline(always)]
const fn is_ecmascript_trim_unit(unit: u16) -> bool {
    matches!(
        unit,
        0x0009 | 0x000a | 0x000b | 0x000c | 0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}
