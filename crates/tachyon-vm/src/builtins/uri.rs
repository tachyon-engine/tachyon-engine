//! ECMAScript URI encoding and decoding globals.

use super::super::*;

const URI_RESERVED: &[u8] = b";/?:@&=+$,#";
const URI_MARK: &[u8] = b"-_.!~*'()";
const HEX: &[u8; 16] = b"0123456789ABCDEF";

impl Isolate {
    /// Executes one URI global after its argument has crossed ToPrimitive with a string hint.
    pub(crate) fn global_uri_primitive_value(
        &mut self,
        function: GlobalUriFunction,
        argument: Value,
    ) -> Result<Value, ExecutionError> {
        let units = self.primitive_string_units(argument)?;
        let output = if function.is_encode() {
            encode_uri_units(&units, function.is_component())?
        } else {
            decode_uri_units(&units, function.is_component())?
        };
        let string = JsString::try_from_utf16(&output).map_err(ExecutionError::ConstantString)?;
        self.allocate_runtime_string(string)
    }

    /// Executes one URI global over an argument that is already known not to be an object.
    pub(crate) fn global_uri_value(
        &mut self,
        function: GlobalUriFunction,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.global_uri_primitive_value(function, argument)
    }
}

/// Percent-encodes UTF-16 input after validating and combining surrogate pairs.
fn encode_uri_units(units: &[u16], component: bool) -> Result<Vec<u16>, ExecutionError> {
    let capacity = encoded_uri_capacity(units, component)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unit <= 0x7f && is_uri_unescaped(unit as u8, component) {
            output.push(unit);
            index += 1;
            continue;
        }
        let scalar = decode_utf16_scalar(units, &mut index)?;
        let mut bytes = [0_u8; 4];
        let encoded = char::from_u32(scalar)
            .ok_or(ExecutionError::InvalidUriEncoding)?
            .encode_utf8(&mut bytes);
        append_percent_bytes(encoded.as_bytes(), &mut output);
    }
    debug_assert_eq!(output.len(), capacity);
    Ok(output)
}

/// Computes the exact encoded length while performing the same surrogate validation as emission.
fn encoded_uri_capacity(units: &[u16], component: bool) -> Result<usize, ExecutionError> {
    let mut capacity = 0_usize;
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        let encoded_units = if unit <= 0x7f && is_uri_unescaped(unit as u8, component) {
            index += 1;
            1
        } else {
            let scalar = decode_utf16_scalar(units, &mut index)?;
            char::from_u32(scalar)
                .ok_or(ExecutionError::InvalidUriEncoding)?
                .len_utf8()
                .checked_mul(3)
                .ok_or(ExecutionError::InvalidStringLength)?
        };
        capacity = capacity
            .checked_add(encoded_units)
            .ok_or(ExecutionError::InvalidStringLength)?;
    }
    Ok(capacity)
}

/// Decodes percent-encoded UTF-8 while preserving reserved escapes for decodeURI.
fn decode_uri_units(units: &[u16], component: bool) -> Result<Vec<u16>, ExecutionError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(units.len())
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    let mut index = 0;
    while index < units.len() {
        if units[index] != u16::from(b'%') {
            output.push(units[index]);
            index += 1;
            continue;
        }
        let start = index;
        let first = read_percent_byte(units, &mut index)?;
        if first < 0x80 {
            if !component && URI_RESERVED.contains(&first) {
                output.extend_from_slice(&units[start..index]);
            } else {
                output.push(u16::from(first));
            }
            continue;
        }
        let length = utf8_sequence_length(first).ok_or(ExecutionError::InvalidUriEncoding)?;
        let mut bytes = [0_u8; 4];
        bytes[0] = first;
        for byte in &mut bytes[1..length] {
            *byte = read_percent_byte(units, &mut index)?;
        }
        let text = core::str::from_utf8(&bytes[..length])
            .map_err(|_| ExecutionError::InvalidUriEncoding)?;
        let mut chars = text.chars();
        let scalar = chars.next().ok_or(ExecutionError::InvalidUriEncoding)? as u32;
        if chars.next().is_some() {
            return Err(ExecutionError::InvalidUriEncoding);
        }
        append_utf16_scalar(scalar, &mut output);
    }
    Ok(output)
}

#[inline(always)]
fn is_uri_unescaped(byte: u8, component: bool) -> bool {
    byte.is_ascii_alphanumeric()
        || URI_MARK.contains(&byte)
        || (!component && URI_RESERVED.contains(&byte))
}

#[inline(always)]
fn decode_utf16_scalar(units: &[u16], index: &mut usize) -> Result<u32, ExecutionError> {
    let first = units[*index];
    *index += 1;
    if !(0xd800..=0xdfff).contains(&first) {
        return Ok(u32::from(first));
    }
    if first > 0xdbff {
        return Err(ExecutionError::InvalidUriEncoding);
    }
    let second = *units
        .get(*index)
        .filter(|unit| (0xdc00..=0xdfff).contains(*unit))
        .ok_or(ExecutionError::InvalidUriEncoding)?;
    *index += 1;
    Ok(0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00))
}

#[inline(always)]
fn append_percent_bytes(bytes: &[u8], output: &mut Vec<u16>) {
    for &byte in bytes {
        output.push(u16::from(b'%'));
        output.push(u16::from(HEX[usize::from(byte >> 4)]));
        output.push(u16::from(HEX[usize::from(byte & 0x0f)]));
    }
}

#[inline(always)]
fn read_percent_byte(units: &[u16], index: &mut usize) -> Result<u8, ExecutionError> {
    if units.get(*index) != Some(&u16::from(b'%')) {
        return Err(ExecutionError::InvalidUriEncoding);
    }
    let high = units
        .get(*index + 1)
        .copied()
        .and_then(hex_value)
        .ok_or(ExecutionError::InvalidUriEncoding)?;
    let low = units
        .get(*index + 2)
        .copied()
        .and_then(hex_value)
        .ok_or(ExecutionError::InvalidUriEncoding)?;
    *index += 3;
    Ok((high << 4) | low)
}

#[inline(always)]
fn hex_value(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x39 => Some((unit - 0x30) as u8),
        0x41..=0x46 => Some((unit - 0x41 + 10) as u8),
        0x61..=0x66 => Some((unit - 0x61 + 10) as u8),
        _ => None,
    }
}

#[inline(always)]
const fn utf8_sequence_length(first: u8) -> Option<usize> {
    match first {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

#[inline(always)]
fn append_utf16_scalar(scalar: u32, output: &mut Vec<u16>) {
    if scalar <= 0xffff {
        output.push(scalar as u16);
        return;
    }
    let adjusted = scalar - 0x10000;
    output.push(0xd800 | (adjusted >> 10) as u16);
    output.push(0xdc00 | (adjusted & 0x3ff) as u16);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_codecs_distinguish_reserved_characters_and_components() {
        let encoded = encode_uri_units(&['/' as u16, 0x00e9], false).unwrap();
        assert_eq!(String::from_utf16(&encoded).unwrap(), "/%C3%A9");
        let component = encode_uri_units(&['/' as u16, 0x00e9], true).unwrap();
        assert_eq!(String::from_utf16(&component).unwrap(), "%2F%C3%A9");
        assert_eq!(
            String::from_utf16(&decode_uri_units(&component, false).unwrap()).unwrap(),
            "%2Fé"
        );
        assert_eq!(
            String::from_utf16(&decode_uri_units(&component, true).unwrap()).unwrap(),
            "/é"
        );
    }

    #[test]
    fn uri_codecs_validate_surrogates_and_utf8() {
        let pair = [0xd83d, 0xde00];
        let encoded = encode_uri_units(&pair, true).unwrap();
        assert_eq!(String::from_utf16(&encoded).unwrap(), "%F0%9F%98%80");
        assert_eq!(decode_uri_units(&encoded, true).unwrap(), pair);
        assert_eq!(
            encode_uri_units(&[0xd800], true),
            Err(ExecutionError::InvalidUriEncoding)
        );
        for malformed in ["%", "%GG", "%C0%80", "%ED%A0%80", "%F4%90%80%80"] {
            let units: Vec<_> = malformed.encode_utf16().collect();
            assert_eq!(
                decode_uri_units(&units, true),
                Err(ExecutionError::InvalidUriEncoding)
            );
        }
    }
}
