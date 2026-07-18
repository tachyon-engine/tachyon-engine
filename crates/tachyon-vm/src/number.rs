//! ECMAScript Number formatting independent of object and native-call dispatch.

use crate::tuning::numbers::RADIX_FORMAT_BUFFER_SIZE;

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NumberFormatError {
    BufferExhausted,
    InvalidDigit,
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
    use super::{NumberFormatError, format_radix};
    use crate::tuning::numbers::RADIX_FORMAT_BUFFER_SIZE;

    fn formatted(value: f64, radix: u8) -> Result<String, NumberFormatError> {
        let mut buffer = [0; RADIX_FORMAT_BUFFER_SIZE];
        format_radix(value, radix, &mut buffer)
            .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
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
