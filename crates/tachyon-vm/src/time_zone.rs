//! Pure ECMA-402 time-zone identifier helpers shared by the VM and Intl providers.

/// Parses the ECMA-402 `UTCOffset` forms accepted as time-zone identifiers.
///
/// Accepted spellings are ASCII `±HH`, `±HHMM`, and `±HH:MM`. DateTimeFormat offset
/// identifiers intentionally reject U+2212 MINUS SIGN. Callers canonicalize every zero offset to
/// `+00:00`, so negative zero is intentionally not preserved in the numeric result.
#[must_use]
pub fn parse_offset_time_zone_minutes(identifier: &str) -> Option<i32> {
    let (negative, digits) = if let Some(digits) = identifier.strip_prefix('+') {
        (false, digits)
    } else {
        (true, identifier.strip_prefix('-')?)
    };
    let (hour_digits, minute_digits) = match digits.len() {
        2 => (digits, "00"),
        4 => (&digits[..2], &digits[2..]),
        5 if digits.as_bytes()[2] == b':' => (&digits[..2], &digits[3..]),
        _ => return None,
    };
    let hours = parse_two_ascii_digits(hour_digits)?;
    let minutes = parse_two_ascii_digits(minute_digits)?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let absolute = hours * 60 + minutes;
    Some(if negative { -absolute } else { absolute })
}

/// Returns the canonical `±HH:MM` spelling for one valid ECMA-402 offset identifier.
#[must_use]
pub fn canonicalize_offset_time_zone_identifier(identifier: &str) -> Option<Box<str>> {
    let minutes = parse_offset_time_zone_minutes(identifier)?;
    let absolute = minutes.unsigned_abs();
    let sign = if minutes < 0 { '-' } else { '+' };
    Some(format!("{sign}{:02}:{:02}", absolute / 60, absolute % 60).into_boxed_str())
}

#[inline(always)]
fn parse_two_ascii_digits(value: &str) -> Option<i32> {
    let [high, low] = value.as_bytes() else {
        return None;
    };
    if !high.is_ascii_digit() || !low.is_ascii_digit() {
        return None;
    }
    Some(i32::from(high - b'0') * 10 + i32::from(low - b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_canonicalizes_offset_time_zone_identifiers() {
        for (input, minutes, canonical) in [
            ("+03", 180, "+03:00"),
            ("+2300", 1_380, "+23:00"),
            ("-07:56", -476, "-07:56"),
            ("-00", 0, "+00:00"),
        ] {
            assert_eq!(parse_offset_time_zone_minutes(input), Some(minutes));
            assert_eq!(
                canonicalize_offset_time_zone_identifier(input).as_deref(),
                Some(canonical)
            );
        }
    }

    #[test]
    fn rejects_non_offset_and_out_of_range_identifiers() {
        for invalid in [
            "UTC", "−05", "+3", "+24", "+23:0", "+2400", "+12:60", "+123", "+12:345",
        ] {
            assert_eq!(parse_offset_time_zone_minutes(invalid), None, "{invalid}");
            assert_eq!(
                canonicalize_offset_time_zone_identifier(invalid),
                None,
                "{invalid}"
            );
        }
    }
}
