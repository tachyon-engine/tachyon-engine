//! Primitive String prototype native methods.

use super::super::*;

impl Isolate {
    /// Implements String.prototype.replace for primitive strings and branded RegExp values.
    pub(crate) fn string_replace(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.string_primitive_value(site.this_value)?;
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let replacement = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(search) && self.regexp_data(search).is_ok() {
            if self.is_callable_value(replacement)? {
                let (input_units, matches) =
                    self.regexp_functional_replace_matches(search, receiver)?;
                return self.begin_regexp_functional_replace(
                    NativeContinuationSite {
                        caller_base: site.caller_base,
                        destination: site.destination,
                        call_site: site.call_site,
                    },
                    search,
                    receiver,
                    replacement,
                    input_units,
                    matches,
                );
            }
            let result = self.regexp_replace_values(search, receiver, replacement)?;
            return self.write(site.caller_base, site.destination, result);
        }
        let input_units = self.string_receiver_units(receiver)?;
        let search_units = self.primitive_string_units(search)?;
        let Some(index) = find_code_units(&input_units, &search_units) else {
            return self.write(site.caller_base, site.destination, receiver);
        };
        if self.is_callable_value(replacement)? {
            let end = index
                .checked_add(search_units.len())
                .ok_or(ExecutionError::InvalidStringLength)?;
            return self.begin_string_functional_replace(
                site,
                receiver,
                replacement,
                input_units,
                index,
                end,
            );
        }
        let replacement_units = self.primitive_string_units(replacement)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(
                input_units
                    .len()
                    .saturating_add(replacement_units.len())
                    .saturating_sub(search_units.len()),
            )
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        output.extend_from_slice(&input_units[..index]);
        output.extend_from_slice(&replacement_units);
        output.extend_from_slice(&input_units[index + search_units.len()..]);
        let result = self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )?;
        self.write(site.caller_base, site.destination, result)
    }
    /// Implements String.prototype.match using the standard RegExp fallback.
    pub(crate) fn string_match(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let receiver = self.string_primitive_value(site.this_value)?;
        let pattern = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let regexp = if self.is_object_value(pattern) && self.regexp_data(pattern).is_ok() {
            pattern
        } else {
            self.create_regexp_for_string_search(pattern)?
        };
        let flags_value = self.regexp_data(regexp)?.1;
        let flags = self.regexp_flags(flags_value)?;
        if flags.global {
            return self.regexp_match_values(regexp, receiver);
        }
        let state = self.allocate_regexp_exec_state(regexp, receiver, 0)?;
        let outcome = self.regexp_builtin_exec(regexp, receiver, state, 0)?;
        Ok(outcome.value)
    }
    /// Returns the primitive String receiver required by String.prototype.toString and valueOf.
    pub(crate) fn string_primitive_value(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        if self.is_string_value(receiver) {
            Ok(receiver)
        } else {
            let raw = receiver
                .as_heap_ref()
                .ok_or(ExecutionError::NotObject(receiver))?;
            let string = self
                .heap
                .checked_reference(raw, self.types.string_object)
                .map_err(|_| ExecutionError::NotObject(receiver))?;
            self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(string, self.types.string_object)
                        .map(|string| string.string_data)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })
        }
    }

    /// Uses newTarget's prototype when constructing a String wrapper.
    pub(crate) fn box_string_from_constructor(
        &mut self,
        string: Value,
        new_target: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .constructor_prototype_value(new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(new_target).ok().and_then(|realm| {
                    self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::String)
                })
            })
            .unwrap_or_else(|| {
                self.realm
                    .string_prototype
                    .expect("String prototype initializes before construction")
            });
        self.allocate_string_object(string, prototype, AllocationSpace::Young)
    }

    /// Checks whether every UTF-16 surrogate belongs to a valid adjacent pair.
    pub(crate) fn string_is_well_formed(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(receiver)?;
        Ok(boolean_value(utf16_is_well_formed(&units)))
    }

    /// Replaces every unpaired UTF-16 surrogate with U+FFFD, preserving valid pairs verbatim.
    pub(crate) fn string_to_well_formed(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        let units = self.string_receiver_units(receiver)?;
        if utf16_is_well_formed(&units) {
            return Ok(receiver);
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(units.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let mut index = 0;
        while let Some(&unit) = units.get(index) {
            if (0xd800..=0xdbff).contains(&unit)
                && units
                    .get(index + 1)
                    .is_some_and(|next| (0xdc00..=0xdfff).contains(next))
            {
                output.extend_from_slice(&units[index..index + 2]);
                index += 2;
            } else {
                output.push(if (0xd800..=0xdfff).contains(&unit) {
                    0xfffd
                } else {
                    unit
                });
                index += 1;
            }
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Applies Unicode Default Case Conversion to an already primitive-coerced receiver.
    pub(crate) fn string_case_primitive_value(
        &mut self,
        receiver: Value,
        uppercase: bool,
    ) -> Result<Value, ExecutionError> {
        let units = self.primitive_string_units(receiver)?;
        let output = case_map_utf16(&units, uppercase)?;
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
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
        let receiver = self.string_primitive_value(site.this_value)?;
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
        let receiver = self.string_primitive_value(receiver)?;
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

/// Finds the first exact UTF-16 subsequence, including an empty search string.
fn find_code_units(input: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    input
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Converts valid UTF-16 segments and preserves unpaired surrogate code units verbatim.
fn case_map_utf16(units: &[u16], uppercase: bool) -> Result<Vec<u16>, ExecutionError> {
    if let Ok(text) = String::from_utf16(units) {
        let mapped = if uppercase {
            text.to_uppercase()
        } else {
            text.to_lowercase()
        };
        return utf8_to_exact_utf16(&mapped);
    }
    let utf8_capacity = units
        .len()
        .checked_mul(3)
        .ok_or(ExecutionError::InvalidStringLength)?;
    let mut segment = String::new();
    segment
        .try_reserve_exact(utf8_capacity)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(units.len())
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    for scalar in char::decode_utf16(units.iter().copied()) {
        match scalar {
            Ok(character) => segment.push(character),
            Err(surrogate) => {
                append_case_segment(&mut output, &mut segment, uppercase)?;
                output.push(surrogate.unpaired_surrogate());
            }
        }
    }
    append_case_segment(&mut output, &mut segment, uppercase)?;
    Ok(output)
}

/// Maps one valid UTF-8 segment before appending its exact UTF-16 expansion.
fn append_case_segment(
    output: &mut Vec<u16>,
    segment: &mut String,
    uppercase: bool,
) -> Result<(), ExecutionError> {
    if segment.is_empty() {
        return Ok(());
    }
    let mapped = if uppercase {
        segment.to_uppercase()
    } else {
        segment.to_lowercase()
    };
    let mapped_units = mapped.encode_utf16().count();
    output
        .try_reserve_exact(mapped_units)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    output.extend(mapped.encode_utf16());
    segment.clear();
    Ok(())
}

/// Materializes a valid UTF-8 string as one exact-capacity UTF-16 allocation.
fn utf8_to_exact_utf16(text: &str) -> Result<Vec<u16>, ExecutionError> {
    let capacity = text.encode_utf16().count();
    let mut units = Vec::new();
    units
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    units.extend(text.encode_utf16());
    debug_assert_eq!(units.len(), capacity);
    Ok(units)
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
fn utf16_is_well_formed(units: &[u16]) -> bool {
    let mut index = 0;
    while let Some(&unit) = units.get(index) {
        if (0xd800..=0xdbff).contains(&unit) {
            if !units
                .get(index + 1)
                .is_some_and(|next| (0xdc00..=0xdfff).contains(next))
            {
                return false;
            }
            index += 2;
        } else if (0xdc00..=0xdfff).contains(&unit) {
            return false;
        } else {
            index += 1;
        }
    }
    true
}

#[inline(always)]
const fn is_ecmascript_trim_unit(unit: u16) -> bool {
    matches!(
        unit,
        0x0009 | 0x000a | 0x000b | 0x000c | 0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}
