//! UTF-16 JSON parsing and intrinsic entry points.

use super::super::*;

mod stringify;
pub(crate) use stringify::PendingJsonStringify;

const MAX_JSON_DEPTH: u32 = 256;
const MAX_JSON_GAP_UNITS: usize = 10;

#[derive(Clone, Copy, Debug)]
struct JsonIndentation {
    gap: [u16; MAX_JSON_GAP_UNITS],
    gap_length: usize,
}

impl JsonIndentation {
    #[inline(always)]
    const fn compact() -> Self {
        Self {
            gap: [0; MAX_JSON_GAP_UNITS],
            gap_length: 0,
        }
    }

    /// Creates the bounded ASCII-space gap selected by a numeric `space` argument.
    #[inline(always)]
    fn spaces(length: usize) -> Self {
        debug_assert!(length <= MAX_JSON_GAP_UNITS);
        let mut indentation = Self::compact();
        indentation.gap[..length].fill(u16::from(b' '));
        indentation.gap_length = length;
        indentation
    }

    #[inline(always)]
    fn is_compact(self) -> bool {
        self.gap_length == 0
    }

    /// Appends one newline followed by the gap repeated for the requested nesting depth.
    fn append_line_indent(self, depth: usize, output: &mut Vec<u16>) -> Result<(), ExecutionError> {
        let indentation_length = self
            .gap_length
            .checked_mul(depth)
            .and_then(|length| length.checked_add(1))
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        output
            .try_reserve(indentation_length)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        output.push(u16::from(b'\n'));
        for _ in 0..depth {
            output.extend_from_slice(&self.gap[..self.gap_length]);
        }
        Ok(())
    }
}

impl Isolate {
    /// Converts JSON.parse text through the shared resumable ToPrimitive path when required.
    pub(crate) fn begin_json_parse(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let text = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let reviver = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(text) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::JsonParseText,
                site.caller_base,
                site.destination,
                reviver,
                text,
                site.call_site,
            );
        }
        self.finish_json_parse_text(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            reviver,
            text,
        )
    }

    /// Parses one already-primitive text value and publishes the unfiltered result.
    pub(crate) fn finish_json_parse_text(
        &mut self,
        site: NativeContinuationSite,
        reviver: Value,
        text: Value,
    ) -> Result<(), ExecutionError> {
        let mut units = Vec::new();
        self.append_primitive_string_units(text, &mut units)?;
        let mut parser = JsonParser {
            isolate: self,
            units: &units,
            index: 0,
        };
        let value = parser.parse_value(0)?;
        parser.skip_space();
        if parser.index == parser.units.len() {
            if self.is_callable_value(reviver)? && !self.is_object_value(value) {
                return self.call_json_parse_root_reviver(site, reviver, value);
            }
            self.write(site.caller_base, site.destination, value)
        } else {
            Err(ExecutionError::InvalidJsonText)
        }
    }

    /// Creates the spec root holder and calls a reviver for one primitive parse result.
    fn call_json_parse_root_reviver(
        &mut self,
        site: NativeContinuationSite,
        reviver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let empty = self.allocate_runtime_string(
            JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let empty_atom = self.property_key_atom(empty)?;
        let wrapper = self.create_ordinary_object()?;
        self.set_own_data_property(wrapper, empty_atom, value)?;
        let prefix = self.create_apply_argument_prefix(reviver, wrapper, vec![empty, value])?;
        self.fiber
            .completions
            .push_native(NativeContinuation::json_parse_reviver(
                site, reviver, wrapper,
            ))
            .map_err(Self::completion_stack_error)?;
        self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: reviver,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 2,
            argument_count: 2,
            this_value: wrapper,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        })?;
        if let Some(frame) = self.fiber.frames.last_mut()
            && frame.call_site == Some(site.call_site)
        {
            frame.return_register = None;
            frame.return_continuation = true;
        }
        Ok(())
    }

    fn json_is_string(&mut self, value: Value) -> bool {
        value
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.string).is_ok())
    }

    fn json_is_symbol(&mut self, value: Value) -> bool {
        value
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.symbol).is_ok())
    }

    /// Quotes a managed string while preserving paired surrogates and escaping lone UTF-16 units.
    fn json_quote_string(
        &mut self,
        value: Value,
        output: &mut Vec<u16>,
    ) -> Result<(), ExecutionError> {
        let mut units = Vec::new();
        self.append_primitive_string_units(value, &mut units)?;
        append_json_quoted_units(&units, output)
    }
}

/// Escapes one code-unit sequence according to QuoteJSONString, retaining valid surrogate pairs.
fn append_json_quoted_units(units: &[u16], output: &mut Vec<u16>) -> Result<(), ExecutionError> {
    output.push(u16::from(b'\"'));
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        match unit {
            0x08 => output.extend([u16::from(b'\\'), u16::from(b'b')]),
            0x09 => output.extend([u16::from(b'\\'), u16::from(b't')]),
            0x0a => output.extend([u16::from(b'\\'), u16::from(b'n')]),
            0x0c => output.extend([u16::from(b'\\'), u16::from(b'f')]),
            0x0d => output.extend([u16::from(b'\\'), u16::from(b'r')]),
            0x00..=0x1f | 34 | 92 => append_json_escape(unit, output),
            0xd800..=0xdbff
                if units
                    .get(index + 1)
                    .is_some_and(|next| (0xdc00..=0xdfff).contains(next)) =>
            {
                output.push(unit);
                output.push(units[index + 1]);
                index += 1;
            }
            0xd800..=0xdfff => append_json_escape(unit, output),
            _ => output.push(unit),
        }
        index += 1;
    }
    output.push(u16::from(b'\"'));
    Ok(())
}

#[inline(always)]
fn append_json_escape(unit: u16, output: &mut Vec<u16>) {
    match unit {
        34 => output.extend([u16::from(b'\\'), u16::from(b'\"')]),
        92 => output.extend([u16::from(b'\\'), u16::from(b'\\')]),
        _ => {
            output.extend([u16::from(b'\\'), u16::from(b'u')]);
            for shift in [12, 8, 4, 0] {
                let digit = ((unit >> shift) & 0x0f) as u8;
                output.push(u16::from(if digit < 10 {
                    b'0' + digit
                } else {
                    b'a' + digit - 10
                }));
            }
        }
    }
}

struct JsonParser<'a> {
    isolate: &'a mut Isolate,
    units: &'a [u16],
    index: usize,
}

impl JsonParser<'_> {
    /// Parses one JSON value and recursively materializes object and Array identities.
    fn parse_value(&mut self, depth: u32) -> Result<Value, ExecutionError> {
        if depth > MAX_JSON_DEPTH {
            return Err(ExecutionError::InvalidJsonText);
        }
        self.skip_space();
        match self.peek() {
            Some(110) => self.parse_literal(b"null", Value::from_immediate(Immediate::Null)),
            Some(102) => self.parse_literal(b"false", Value::from_immediate(Immediate::False)),
            Some(116) => self.parse_literal(b"true", Value::from_immediate(Immediate::True)),
            Some(34) => self.parse_string(),
            Some(45 | 48..=57) => self.parse_number(),
            Some(91) => self.parse_array(depth + 1),
            Some(123) => self.parse_object(depth + 1),
            _ => Err(ExecutionError::InvalidJsonText),
        }
    }

    /// Parses a JSON Array in source order, publishing each child before reading the next one.
    fn parse_array(&mut self, depth: u32) -> Result<Value, ExecutionError> {
        self.expect(b'[')?;
        let prototype = self
            .isolate
            .realm
            .array_prototype
            .expect("Array prototype initializes before JSON.parse");
        let array = self.isolate.create_array_object_with_prototype(prototype)?;
        self.skip_space();
        let mut length = 0_u64;
        if self.consume(b']') {
            self.set_array_length(array, length)?;
            return Ok(array);
        }
        loop {
            let value = self.parse_value(depth)?;
            let key = self.isolate.safe_integer_property_atom(length)?;
            self.isolate.set_own_data_property(array, key, value)?;
            length = length
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
            self.skip_space();
            if self.consume(b']') {
                self.set_array_length(array, length)?;
                return Ok(array);
            }
            self.expect(b',')?;
        }
    }

    /// Parses a JSON object, using ordinary data property overwrite semantics for duplicate names.
    fn parse_object(&mut self, depth: u32) -> Result<Value, ExecutionError> {
        self.expect(b'{')?;
        let object = self.isolate.create_ordinary_object()?;
        self.skip_space();
        if self.consume(b'}') {
            return Ok(object);
        }
        loop {
            self.skip_space();
            let key = self.parse_string()?;
            let key = self.isolate.property_key_atom(key)?;
            self.skip_space();
            self.expect(b':')?;
            let value = self.parse_value(depth)?;
            self.isolate.set_own_data_property(object, key, value)?;
            self.skip_space();
            if self.consume(b'}') {
                return Ok(object);
            }
            self.expect(b',')?;
        }
    }

    /// Decodes JSON's escape grammar into the engine's code-unit preserving string representation.
    fn parse_string(&mut self) -> Result<Value, ExecutionError> {
        self.expect(b'\"')?;
        let mut output = Vec::new();
        while let Some(unit) = self.units.get(self.index).copied() {
            self.index += 1;
            match unit {
                34 => {
                    let string = JsString::try_from_owned_code_units(output)
                        .map_err(ExecutionError::ConstantString)?;
                    return self.isolate.allocate_runtime_string(string);
                }
                0..=0x1f => return Err(ExecutionError::InvalidJsonText),
                92 => output.push(self.parse_escape()?),
                _ => output.push(unit),
            }
        }
        Err(ExecutionError::InvalidJsonText)
    }

    /// Parses an escaped quote, control character, or four-digit UTF-16 escape unit.
    fn parse_escape(&mut self) -> Result<u16, ExecutionError> {
        let Some(escape) = self.units.get(self.index).copied() else {
            return Err(ExecutionError::InvalidJsonText);
        };
        self.index += 1;
        match escape {
            34 | 47 | 92 => Ok(escape),
            98 => Ok(0x0008),
            102 => Ok(0x000c),
            110 => Ok(0x000a),
            114 => Ok(0x000d),
            116 => Ok(0x0009),
            117 => {
                let digits = self
                    .units
                    .get(self.index..self.index + 4)
                    .ok_or(ExecutionError::InvalidJsonText)?;
                let mut value = 0_u16;
                for digit in digits {
                    value =
                        (value << 4) | hex_value(*digit).ok_or(ExecutionError::InvalidJsonText)?;
                }
                self.index += 4;
                Ok(value)
            }
            _ => Err(ExecutionError::InvalidJsonText),
        }
    }

    /// Parses JSON's decimal-only number syntax before converting its ASCII token to binary64.
    fn parse_number(&mut self) -> Result<Value, ExecutionError> {
        let start = self.index;
        self.consume(b'-');
        match self.peek() {
            Some(48) => self.index += 1,
            Some(49..=57) => {
                self.consume_digits();
            }
            _ => return Err(ExecutionError::InvalidJsonText),
        }
        if self.consume(b'.') && !self.consume_digits() {
            return Err(ExecutionError::InvalidJsonText);
        }
        if matches!(self.peek(), Some(69 | 101)) {
            self.index += 1;
            let _ = self.consume(b'+') || self.consume(b'-');
            if !self.consume_digits() {
                return Err(ExecutionError::InvalidJsonText);
            }
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(self.index - start)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        bytes.extend(self.units[start..self.index].iter().map(|unit| *unit as u8));
        let text = core::str::from_utf8(&bytes).map_err(|_| ExecutionError::InvalidJsonText)?;
        let number = text
            .parse::<f64>()
            .map_err(|_| ExecutionError::InvalidJsonText)?;
        Ok(Value::from_f64(number))
    }

    fn parse_literal(&mut self, literal: &[u8], value: Value) -> Result<Value, ExecutionError> {
        let end = self.index + literal.len();
        if self.units.get(self.index..end).is_some_and(|candidate| {
            candidate
                .iter()
                .copied()
                .eq(literal.iter().copied().map(u16::from))
        }) {
            self.index = end;
            Ok(value)
        } else {
            Err(ExecutionError::InvalidJsonText)
        }
    }

    fn set_array_length(&mut self, array: Value, length: u64) -> Result<(), ExecutionError> {
        let length_atom = self.isolate.length_atom()?;
        self.isolate
            .set_own_data_property(array, length_atom, safe_integer_value(length))
    }

    #[inline(always)]
    fn peek(&self) -> Option<u16> {
        self.units.get(self.index).copied()
    }

    #[inline(always)]
    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(u16::from(expected)) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ExecutionError> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(ExecutionError::InvalidJsonText)
        }
    }

    #[inline(always)]
    fn consume_digits(&mut self) -> bool {
        let start = self.index;
        while matches!(self.peek(), Some(48..=57)) {
            self.index += 1;
        }
        self.index != start
    }

    #[inline(always)]
    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(0x20 | 0x09 | 0x0a | 0x0d)) {
            self.index += 1;
        }
    }
}

#[inline(always)]
const fn hex_value(unit: u16) -> Option<u16> {
    match unit {
        48..=57 => Some(unit - 48),
        97..=102 => Some(unit - 97 + 10),
        65..=70 => Some(unit - 65 + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
