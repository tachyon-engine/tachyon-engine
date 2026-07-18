//! ECMAScript Number formatting independent of object and native-call dispatch.

use core::cmp::Ordering;

use crate::tuning::numbers::{
    DECIMAL_BIGINT_LIMBS, EXPONENTIAL_FORMAT_BUFFER_SIZE, MAX_DECIMAL_FRACTION_DIGITS,
    RADIX_FORMAT_BUFFER_SIZE,
};

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NumberFormatError {
    BufferExhausted,
    InvalidDigit,
}

#[derive(Clone)]
struct BigUnsigned {
    limbs: [u32; DECIMAL_BIGINT_LIMBS],
    len: usize,
}

impl BigUnsigned {
    #[inline]
    fn from_u64(value: u64) -> Self {
        let mut limbs = [0; DECIMAL_BIGINT_LIMBS];
        limbs[0] = value as u32;
        limbs[1] = (value >> 32) as u32;
        Self {
            limbs,
            len: if limbs[1] == 0 { 1 } else { 2 },
        }
    }

    /// Multiplies by one small factor while preserving a normalized limb length.
    fn multiply_small(&mut self, factor: u32) -> Result<(), NumberFormatError> {
        let mut carry = 0_u64;
        for limb in &mut self.limbs[..self.len] {
            let product = u64::from(*limb) * u64::from(factor) + carry;
            *limb = product as u32;
            carry = product >> 32;
        }
        if carry != 0 {
            let limb = self
                .limbs
                .get_mut(self.len)
                .ok_or(NumberFormatError::BufferExhausted)?;
            *limb = carry as u32;
            self.len += 1;
        }
        Ok(())
    }

    /// Shifts left without allocation; the tuning bound covers all binary64 normalized ratios.
    fn shift_left(&mut self, bits: usize) -> Result<(), NumberFormatError> {
        let words = bits / 32;
        let remainder = bits % 32;
        let extra = usize::from(remainder != 0);
        let new_len = self
            .len
            .checked_add(words)
            .and_then(|len| len.checked_add(extra))
            .ok_or(NumberFormatError::BufferExhausted)?;
        if new_len > self.limbs.len() {
            return Err(NumberFormatError::BufferExhausted);
        }
        if words != 0 {
            self.limbs.copy_within(..self.len, words);
            self.limbs[..words].fill(0);
            self.len += words;
        }
        if remainder != 0 {
            let mut carry = 0_u32;
            for limb in &mut self.limbs[..self.len] {
                let next = *limb >> (32 - remainder);
                *limb = (*limb << remainder) | carry;
                carry = next;
            }
            self.limbs[self.len] = carry;
            self.len += usize::from(carry != 0);
        }
        Ok(())
    }

    #[inline]
    fn compare(&self, other: &Self) -> Ordering {
        match self.len.cmp(&other.len) {
            Ordering::Equal => self.limbs[..self.len]
                .iter()
                .rev()
                .cmp(other.limbs[..other.len].iter().rev()),
            ordering => ordering,
        }
    }

    /// Subtracts a known-smaller value and trims zero high limbs.
    fn subtract(&mut self, other: &Self) {
        debug_assert!(self.compare(other) != Ordering::Less);
        let mut borrow = 0_u64;
        for index in 0..self.len {
            let left = u64::from(self.limbs[index]);
            let right = u64::from(other.limbs.get(index).copied().unwrap_or(0)) + borrow;
            self.limbs[index] = left.wrapping_sub(right) as u32;
            borrow = u64::from(left < right);
        }
        debug_assert_eq!(borrow, 0);
        while self.len > 1 && self.limbs[self.len - 1] == 0 {
            self.len -= 1;
        }
    }
}

struct ByteCursor<'a> {
    bytes: &'a mut [u8; EXPONENTIAL_FORMAT_BUFFER_SIZE],
    len: usize,
}

impl<'a> ByteCursor<'a> {
    #[inline(always)]
    fn push(&mut self, byte: u8) -> Result<(), NumberFormatError> {
        let slot = self
            .bytes
            .get_mut(self.len)
            .ok_or(NumberFormatError::BufferExhausted)?;
        *slot = byte;
        self.len += 1;
        Ok(())
    }

    /// Emits an explicitly signed decimal exponent without formatting infrastructure.
    fn push_exponent(&mut self, exponent: i16) -> Result<(), NumberFormatError> {
        self.push(b'e')?;
        self.push(if exponent < 0 { b'-' } else { b'+' })?;
        let mut value = exponent.unsigned_abs();
        let mut reversed = [0_u8; 3];
        let mut digits = 0;
        loop {
            reversed[digits] = (value % 10) as u8;
            digits += 1;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        for digit in reversed[..digits].iter().rev() {
            self.push(b'0' + *digit)?;
        }
        Ok(())
    }
}

/// Formats Number.prototype.toExponential into bounded caller-owned storage.
pub(crate) fn format_exponential(
    value: f64,
    fraction_digits: Option<u8>,
    buffer: &mut [u8; EXPONENTIAL_FORMAT_BUFFER_SIZE],
) -> Result<&[u8], NumberFormatError> {
    let mut output = ByteCursor {
        bytes: buffer,
        len: 0,
    };
    if value.is_nan() {
        for byte in b"NaN" {
            output.push(*byte)?;
        }
        return Ok(&output.bytes[..output.len]);
    }
    if value.is_infinite() {
        let bytes = if value.is_sign_negative() {
            b"-Infinity".as_slice()
        } else {
            b"Infinity".as_slice()
        };
        for byte in bytes {
            output.push(*byte)?;
        }
        return Ok(&output.bytes[..output.len]);
    }
    if value == 0.0 {
        write_zero_exponential(&mut output, fraction_digits)?;
        return Ok(&output.bytes[..output.len]);
    }
    let negative = value.is_sign_negative();
    let absolute = value.abs();
    let mut shortest_buffer = ryu_js::Buffer::new();
    let shortest = shortest_buffer.format_finite(absolute).as_bytes();
    let exponent = decimal_exponent(shortest)?;
    if negative {
        output.push(b'-')?;
    }
    match fraction_digits {
        Some(fraction_digits) => {
            write_precision_exponential(absolute, exponent, fraction_digits, &mut output)?;
        }
        None => write_shortest_exponential(shortest, exponent, &mut output)?,
    }
    Ok(&output.bytes[..output.len])
}

/// Derives the normalized decimal exponent from ryu-js's finite ECMAScript representation.
fn decimal_exponent(shortest: &[u8]) -> Result<i16, NumberFormatError> {
    if let Some(marker) = shortest.iter().position(|&byte| byte == b'e') {
        let mut cursor = marker + 1;
        let negative = shortest.get(cursor) == Some(&b'-');
        cursor += usize::from(negative || shortest.get(cursor) == Some(&b'+'));
        let mut exponent = 0_i16;
        for byte in shortest
            .get(cursor..)
            .ok_or(NumberFormatError::InvalidDigit)?
        {
            if !byte.is_ascii_digit() {
                return Err(NumberFormatError::InvalidDigit);
            }
            exponent = exponent
                .checked_mul(10)
                .and_then(|value| value.checked_add(i16::from(*byte - b'0')))
                .ok_or(NumberFormatError::InvalidDigit)?;
        }
        return Ok(if negative { -exponent } else { exponent });
    }
    let point = shortest
        .iter()
        .position(|&byte| byte == b'.')
        .unwrap_or(shortest.len());
    let first = shortest
        .iter()
        .position(|byte| byte.is_ascii_digit() && *byte != b'0')
        .ok_or(NumberFormatError::InvalidDigit)?;
    if first < point {
        i16::try_from(point - first - 1).map_err(|_| NumberFormatError::InvalidDigit)
    } else {
        i16::try_from(first - point)
            .map(|distance| -distance)
            .map_err(|_| NumberFormatError::InvalidDigit)
    }
}

/// Writes zero with the exact requested fractional width and canonical positive exponent.
fn write_zero_exponential(
    output: &mut ByteCursor<'_>,
    fraction_digits: Option<u8>,
) -> Result<(), NumberFormatError> {
    output.push(b'0')?;
    if let Some(fraction_digits) = fraction_digits
        && fraction_digits != 0
    {
        output.push(b'.')?;
        for _ in 0..fraction_digits {
            output.push(b'0')?;
        }
    }
    output.push_exponent(0)
}

/// Normalizes ryu-js's shortest round-trip digits without allocating or changing them.
fn write_shortest_exponential(
    shortest: &[u8],
    exponent: i16,
    output: &mut ByteCursor<'_>,
) -> Result<(), NumberFormatError> {
    let mantissa_end = shortest
        .iter()
        .position(|&byte| byte == b'e')
        .unwrap_or(shortest.len());
    let mut digits = [0_u8; 17];
    let mut count = 0;
    let mut pending_zeros = 0;
    for byte in &shortest[..mantissa_end] {
        if byte.is_ascii_digit() {
            if *byte == b'0' {
                pending_zeros += usize::from(count != 0);
            } else {
                while pending_zeros != 0 {
                    *digits
                        .get_mut(count)
                        .ok_or(NumberFormatError::BufferExhausted)? = b'0';
                    count += 1;
                    pending_zeros -= 1;
                }
                *digits
                    .get_mut(count)
                    .ok_or(NumberFormatError::BufferExhausted)? = *byte;
                count += 1;
            }
        } else if *byte != b'.' {
            return Err(NumberFormatError::InvalidDigit);
        }
    }
    output.push(digits[0])?;
    if count > 1 {
        output.push(b'.')?;
        for byte in &digits[1..count] {
            output.push(*byte)?;
        }
    }
    output.push_exponent(exponent)
}

/// Generates exact significant digits from the binary rational and performs ties-up rounding.
fn write_precision_exponential(
    value: f64,
    mut exponent: i16,
    fraction_digits: u8,
    output: &mut ByteCursor<'_>,
) -> Result<(), NumberFormatError> {
    debug_assert!(value.is_finite() && value > 0.0);
    debug_assert!(usize::from(fraction_digits) <= MAX_DECIMAL_FRACTION_DIGITS);
    let (mut numerator, denominator) = normalized_ratio(value, exponent)?;
    let digit_count = usize::from(fraction_digits) + 1;
    let mut digits = [0_u8; MAX_DECIMAL_FRACTION_DIGITS + 1];
    for (index, digit) in digits[..digit_count].iter_mut().enumerate() {
        while numerator.compare(&denominator) != Ordering::Less {
            numerator.subtract(&denominator);
            *digit += 1;
        }
        if *digit > 9 {
            return Err(NumberFormatError::InvalidDigit);
        }
        if index + 1 != digit_count {
            numerator.multiply_small(10)?;
        }
    }
    let mut doubled_remainder = numerator.clone();
    doubled_remainder.multiply_small(2)?;
    if doubled_remainder.compare(&denominator) != Ordering::Less
        && round_decimal_digits(&mut digits[..digit_count])
    {
        exponent += 1;
    }
    output.push(b'0' + digits[0])?;
    if fraction_digits != 0 {
        output.push(b'.')?;
        for digit in &digits[1..digit_count] {
            output.push(b'0' + *digit)?;
        }
    }
    output.push_exponent(exponent)
}

/// Builds value / 10^exponent exactly as two bounded base-2^32 integers.
fn normalized_ratio(
    value: f64,
    exponent: i16,
) -> Result<(BigUnsigned, BigUnsigned), NumberFormatError> {
    let bits = value.to_bits();
    let encoded_exponent = ((bits >> 52) & 0x7ff) as i32;
    let encoded_mantissa = bits & ((1_u64 << 52) - 1);
    let (binary_exponent, mantissa) = if encoded_exponent == 0 {
        (-1_074, encoded_mantissa)
    } else {
        (
            encoded_exponent - 1_023 - 52,
            encoded_mantissa | (1_u64 << 52),
        )
    };
    let mut numerator = BigUnsigned::from_u64(mantissa);
    let mut denominator = BigUnsigned::from_u64(1);
    if exponent >= 0 {
        for _ in 0..exponent {
            denominator.multiply_small(5)?;
        }
        apply_binary_shift(
            &mut numerator,
            &mut denominator,
            binary_exponent - i32::from(exponent),
        )?;
    } else {
        let magnitude = i32::from(-exponent);
        for _ in 0..magnitude {
            numerator.multiply_small(5)?;
        }
        apply_binary_shift(
            &mut numerator,
            &mut denominator,
            binary_exponent + magnitude,
        )?;
    }
    debug_assert!(numerator.compare(&denominator) != Ordering::Less);
    let mut ten_denominator = denominator.clone();
    ten_denominator.multiply_small(10)?;
    debug_assert_eq!(numerator.compare(&ten_denominator), Ordering::Less);
    Ok((numerator, denominator))
}

#[inline]
fn apply_binary_shift(
    numerator: &mut BigUnsigned,
    denominator: &mut BigUnsigned,
    shift: i32,
) -> Result<(), NumberFormatError> {
    if shift >= 0 {
        numerator.shift_left(shift as usize)
    } else {
        denominator.shift_left((-shift) as usize)
    }
}

/// Propagates a decimal carry and reports whether it increased the exponent.
fn round_decimal_digits(digits: &mut [u8]) -> bool {
    for digit in digits.iter_mut().rev() {
        if *digit != 9 {
            *digit += 1;
            return false;
        }
        *digit = 0;
    }
    digits[0] = 1;
    true
}

/// Formats one finite nonzero IEEE-754 value in radix 2..=36 with shortest round-trip digits.
///
/// This follows the boundary-delta algorithm used by V8-derived engines: fractional digits stop
/// once either adjacent representable double is farther away than the remaining suffix, with
/// ties rounded to even. The caller owns fixed scratch storage, so formatting performs no heap
/// allocation and cannot retain a pointer after returning.
pub(crate) fn format_radix(
    mut value: f64,
    radix: u8,
    buffer: &mut [u8; RADIX_FORMAT_BUFFER_SIZE],
) -> Result<&[u8], NumberFormatError> {
    debug_assert!(value.is_finite() && value != 0.0);
    debug_assert!((2..=36).contains(&radix));

    let midpoint = buffer.len() / 2;
    let negative = value.is_sign_negative();
    if negative {
        value = -value;
    }
    let mut integer = value.floor();
    let mut fraction = value - integer;
    let mut fraction_cursor = midpoint;
    let radix_number = f64::from(radix);
    let mut delta = 0.5 * (next_after(value, f64::INFINITY) - value);
    delta = f64::from_bits(1).max(delta);

    if fraction >= delta {
        write_byte(buffer, fraction_cursor, b'.')?;
        fraction_cursor += 1;
        loop {
            fraction *= radix_number;
            delta *= radix_number;
            let digit = fraction as u8;
            write_byte(buffer, fraction_cursor, digit_byte(digit, radix)?)?;
            fraction_cursor += 1;
            fraction -= f64::from(digit);

            if fraction + delta > 1.0 && (fraction > 0.5 || (fraction == 0.5 && digit & 1 != 0)) {
                round_fraction_up(buffer, midpoint, &mut fraction_cursor, radix, &mut integer)?;
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }

    let mut integer_cursor = midpoint;
    while integer_decode_exponent(integer / radix_number) > 0 {
        push_integer_digit(buffer, &mut integer_cursor, b'0')?;
        integer /= radix_number;
    }
    loop {
        let remainder = (integer % radix_number) as u8;
        push_integer_digit(buffer, &mut integer_cursor, digit_byte(remainder, radix)?)?;
        integer = (integer - f64::from(remainder)) / radix_number;
        if integer <= 0.0 {
            break;
        }
    }
    if negative {
        push_integer_digit(buffer, &mut integer_cursor, b'-')?;
    }
    buffer
        .get(integer_cursor..fraction_cursor)
        .ok_or(NumberFormatError::BufferExhausted)
}

/// Advances one finite double toward the requested bound without relying on platform libm.
#[inline(always)]
fn next_after(value: f64, toward: f64) -> f64 {
    if value.is_nan() || toward.is_nan() {
        return f64::NAN;
    }
    if value == toward {
        return value;
    }
    if value == 0.0 {
        return f64::from_bits(1).copysign(toward);
    }
    if toward > value || value > 0.0 {
        f64::from_bits(value.to_bits() + 1)
    } else {
        f64::from_bits(value.to_bits() - 1)
    }
}

/// Returns the power-of-two exponent paired with the 53-bit integer mantissa.
#[inline(always)]
fn integer_decode_exponent(value: f64) -> i16 {
    let encoded = ((value.to_bits() >> 52) & 0x7ff) as i16;
    if encoded == 0 {
        -1_074
    } else {
        encoded - 1_023 - 52
    }
}

#[inline(always)]
fn write_byte(
    buffer: &mut [u8; RADIX_FORMAT_BUFFER_SIZE],
    index: usize,
    byte: u8,
) -> Result<(), NumberFormatError> {
    let slot = buffer
        .get_mut(index)
        .ok_or(NumberFormatError::BufferExhausted)?;
    *slot = byte;
    Ok(())
}

#[inline(always)]
fn push_integer_digit(
    buffer: &mut [u8; RADIX_FORMAT_BUFFER_SIZE],
    cursor: &mut usize,
    byte: u8,
) -> Result<(), NumberFormatError> {
    *cursor = cursor
        .checked_sub(1)
        .ok_or(NumberFormatError::BufferExhausted)?;
    write_byte(buffer, *cursor, byte)
}

#[inline(always)]
fn digit_byte(digit: u8, radix: u8) -> Result<u8, NumberFormatError> {
    if digit >= radix {
        return Err(NumberFormatError::InvalidDigit);
    }
    DIGITS
        .get(digit as usize)
        .copied()
        .ok_or(NumberFormatError::InvalidDigit)
}

/// Propagates a tie-to-even carry through the already emitted fractional suffix.
fn round_fraction_up(
    buffer: &mut [u8; RADIX_FORMAT_BUFFER_SIZE],
    midpoint: usize,
    cursor: &mut usize,
    radix: u8,
    integer: &mut f64,
) -> Result<(), NumberFormatError> {
    loop {
        *cursor = cursor
            .checked_sub(1)
            .ok_or(NumberFormatError::BufferExhausted)?;
        if *cursor == midpoint {
            *integer += 1.0;
            return Ok(());
        }
        let byte = *buffer
            .get(*cursor)
            .ok_or(NumberFormatError::BufferExhausted)?;
        let digit = if byte > b'9' {
            byte - b'a' + 10
        } else {
            byte - b'0'
        };
        if digit + 1 >= radix {
            continue;
        }
        write_byte(buffer, *cursor, digit_byte(digit + 1, radix)?)?;
        *cursor += 1;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::{NumberFormatError, format_exponential, format_radix};
    use crate::tuning::numbers::{EXPONENTIAL_FORMAT_BUFFER_SIZE, RADIX_FORMAT_BUFFER_SIZE};

    fn formatted(value: f64, radix: u8) -> Result<String, NumberFormatError> {
        let mut buffer = [0; RADIX_FORMAT_BUFFER_SIZE];
        format_radix(value, radix, &mut buffer)
            .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn exponential(value: f64, fraction_digits: Option<u8>) -> String {
        let mut buffer = [0; EXPONENTIAL_FORMAT_BUFFER_SIZE];
        String::from_utf8(
            format_exponential(value, fraction_digits, &mut buffer)
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn exponential_formatter_handles_shortest_special_and_zero_forms() {
        assert_eq!(exponential(123.456, None), "1.23456e+2");
        assert_eq!(exponential(100.0, None), "1e+2");
        assert_eq!(exponential(1e20, None), "1e+20");
        assert_eq!(exponential(1.1e-32, None), "1.1e-32");
        assert_eq!(exponential(f64::MAX, None), "1.7976931348623157e+308");
        assert_eq!(exponential(f64::from_bits(1), None), "5e-324");
        assert_eq!(exponential(-0.0, Some(2)), "0.00e+0");
        assert_eq!(exponential(f64::NAN, Some(100)), "NaN");
        assert_eq!(exponential(f64::NEG_INFINITY, Some(100)), "-Infinity");
    }

    #[test]
    fn exponential_formatter_uses_ecmascript_ties_up_and_carry_rules() {
        assert_eq!(exponential(25.0, Some(0)), "3e+1");
        assert_eq!(exponential(123.456, Some(3)), "1.235e+2");
        assert_eq!(exponential(0.9999, Some(0)), "1e+0");
        assert_eq!(exponential(0.9999, Some(3)), "9.999e-1");
    }

    #[test]
    fn exponential_formatter_covers_precision_and_ieee_extremes() {
        assert_eq!(
            exponential(3.0, Some(100)),
            "3.0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e+0"
        );
        assert!(exponential(f64::MAX, Some(100)).ends_with("e+308"));
        assert!(exponential(f64::from_bits(1), Some(100)).ends_with("e-324"));
    }

    #[test]
    fn exponential_scratch_covers_every_finite_binary64_exponent() {
        const MANTISSA_MASK: u64 = (1_u64 << 52) - 1;
        for encoded_exponent in 0_u64..=0x7fe {
            for mantissa in [0, MANTISSA_MASK] {
                let value = f64::from_bits((encoded_exponent << 52) | mantissa);
                if value == 0.0 {
                    continue;
                }
                for precision in [Some(0), Some(100)] {
                    let formatted = exponential(value, precision);
                    assert!(formatted.is_ascii());
                    assert!(formatted.contains('e'));
                }
            }
        }
    }

    #[test]
    fn radix_formatter_handles_sign_integer_and_fraction_boundaries() {
        assert_eq!(formatted(255.0, 16).unwrap(), "ff");
        assert_eq!(formatted(-42.0, 2).unwrap(), "-101010");
        assert_eq!(formatted(0.5, 2).unwrap(), "0.1");
        assert_eq!(formatted(1.5, 2).unwrap(), "1.1");
        assert_eq!(formatted(35.0, 36).unwrap(), "z");
    }

    #[test]
    fn radix_formatter_emits_shortest_binary_round_trip_for_decimal_tenth() {
        assert_eq!(
            formatted(0.1, 2).unwrap(),
            "0.0001100110011001100110011001100110011001100110011001101"
        );
    }

    #[test]
    fn radix_buffer_covers_every_ieee_exponent_and_supported_radix() {
        for radix in 2..=36 {
            for value in [f64::MAX, f64::MIN_POSITIVE, f64::from_bits(1)] {
                let output = formatted(value, radix).unwrap();
                assert!(!output.is_empty());
                assert!(output.is_ascii());
            }
        }
    }
}
