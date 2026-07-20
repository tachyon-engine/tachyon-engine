//! Numeric global functions whose coercion differs from Number static predicates.

use super::super::*;

impl Isolate {
    /// Finishes a numeric global after its argument has crossed ToPrimitive.
    pub(crate) fn global_number_primitive_value(
        &mut self,
        function: GlobalNumberFunction,
        argument: Value,
    ) -> Result<Value, ExecutionError> {
        let result = match function {
            GlobalNumberFunction::IsFinite | GlobalNumberFunction::IsNaN => {
                let converted = self.convert_to_number(argument)?;
                let number = numeric_value(converted)
                    .ok_or(ExecutionError::UnsupportedNumberConversion(argument))?;
                let predicate = if function == GlobalNumberFunction::IsFinite {
                    number.is_finite()
                } else {
                    number.is_nan()
                };
                return Ok(Value::from_immediate(if predicate {
                    Immediate::True
                } else {
                    Immediate::False
                }));
            }
            GlobalNumberFunction::ParseFloat => {
                parse_float_units(&self.primitive_string_units(argument)?)
            }
            GlobalNumberFunction::ParseInt => {
                parse_int_units(&self.primitive_string_units(argument)?, 0)
            }
        };
        Ok(Value::from_f64(result))
    }

    /// Executes one numeric global over the currently available primitive coercion surface.
    pub(crate) fn global_number_value(
        &mut self,
        function: GlobalNumberFunction,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let result = match function {
            GlobalNumberFunction::IsFinite | GlobalNumberFunction::IsNaN => {
                return self.global_number_primitive_value(function, argument);
            }
            GlobalNumberFunction::ParseFloat => {
                parse_float_units(&self.primitive_string_units(argument)?)
            }
            GlobalNumberFunction::ParseInt => {
                let radix = self
                    .call_argument(site, 1)?
                    .filter(|value| value.as_immediate() != Some(Immediate::Undefined))
                    .map(|value| self.convert_to_number(value))
                    .transpose()?
                    .and_then(numeric_value)
                    .map(to_int32)
                    .unwrap_or(0);
                parse_int_units(&self.primitive_string_units(argument)?, radix)
            }
        };
        Ok(Value::from_f64(result))
    }

    /// Materializes primitive ToString code units for prefix numeric parsing.
    fn primitive_string_units(&mut self, value: Value) -> Result<Vec<u16>, ExecutionError> {
        if self.is_object_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        let mut units = Vec::new();
        self.append_primitive_string_units(value, &mut units)?;
        Ok(units)
    }
}

fn parse_float_units(units: &[u16]) -> f64 {
    let Ok(text) = String::from_utf16(units) else {
        return f64::NAN;
    };
    let text = text.trim_start_matches(is_ecmascript_whitespace);
    if let Some(rest) = text.strip_prefix("Infinity") {
        let _ = rest;
        return f64::INFINITY;
    }
    if let Some(rest) = text.strip_prefix("+Infinity") {
        let _ = rest;
        return f64::INFINITY;
    }
    if let Some(rest) = text.strip_prefix("-Infinity") {
        let _ = rest;
        return f64::NEG_INFINITY;
    }
    let bytes = text.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let integer_start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    let mut has_digits = end != integer_start;
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        let fraction_start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        has_digits |= end != fraction_start;
    }
    if !has_digits {
        return f64::NAN;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let exponent = end;
        end += 1;
        if matches!(bytes.get(end), Some(b'+' | b'-')) {
            end += 1;
        }
        let digits = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == digits {
            end = exponent;
        }
    }
    text[..end].parse::<f64>().unwrap_or(f64::NAN)
}

fn parse_int_units(units: &[u16], requested_radix: i32) -> f64 {
    let Ok(text) = String::from_utf16(units) else {
        return f64::NAN;
    };
    let mut text = text.trim_start_matches(is_ecmascript_whitespace);
    let mut sign = 1.0;
    if let Some(rest) = text.strip_prefix('-') {
        sign = -1.0;
        text = rest;
    } else if let Some(rest) = text.strip_prefix('+') {
        text = rest;
    }
    let mut radix = requested_radix;
    if radix != 0 && !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let strip_prefix = radix == 0 || radix == 16;
    if strip_prefix && (text.starts_with("0x") || text.starts_with("0X")) {
        text = &text[2..];
        radix = 16;
    } else if radix == 0 {
        radix = 10;
    }
    let mut value = 0.0;
    let mut consumed = false;
    for byte in text.bytes() {
        let Some(digit) = (byte as char).to_digit(radix as u32) else {
            break;
        };
        consumed = true;
        value = value * f64::from(radix) + f64::from(digit);
    }
    if consumed { sign * value } else { f64::NAN }
}

#[inline]
fn to_int32(number: f64) -> i32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let value = number.trunc().rem_euclid(4_294_967_296.0);
    if value >= 2_147_483_648.0 {
        (value - 4_294_967_296.0) as i32
    } else {
        value as i32
    }
}

#[inline]
fn is_ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_parsers_follow_ecmascript_prefix_and_radix_rules() {
        let units = |text: &str| text.encode_utf16().collect::<Vec<_>>();
        assert_eq!(parse_float_units(&units("  -1.25e2tail")), -125.0);
        assert_eq!(parse_float_units(&units("Infinity!")), f64::INFINITY);
        assert!(parse_float_units(&units("+.")).is_nan());
        assert_eq!(parse_int_units(&units(" -0x10tail"), 0), -16.0);
        assert_eq!(parse_int_units(&units("11"), 2), 3.0);
        assert!(parse_int_units(&units("10"), 1).is_nan());
    }
}
