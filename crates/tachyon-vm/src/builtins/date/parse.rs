//! ECMAScript Date Time String Format parsing without host timezone access.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ParsedDateTime {
    Utc(f64),
    Local([f64; 7]),
}

/// Parses the normative ISO grammar, then the engine's own required Date string formats.
pub(super) fn parse_date_time(units: &[u16]) -> Option<ParsedDateTime> {
    parse_iso_date_time(units).or_else(|| parse_engine_date_string(units))
}

/// Parses the specification's ISO date-time format and preserves local interpretation explicitly.
pub(super) fn parse_iso_date_time(units: &[u16]) -> Option<ParsedDateTime> {
    let mut parser = IsoParser { units, cursor: 0 };
    let year = parser.year()?;
    let mut month = 1_u32;
    let mut day = 1_u32;
    let mut has_complete_date = false;
    if parser.consume(b'-') {
        month = parser.digits(2)?;
        if parser.consume(b'-') {
            day = parser.digits(2)?;
            has_complete_date = true;
        }
    }
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }

    let mut fields = [
        year as f64,
        (month - 1) as f64,
        day as f64,
        0.0,
        0.0,
        0.0,
        0.0,
    ];
    if parser.at_end() {
        return Some(ParsedDateTime::Utc(make_utc_date(fields)));
    }
    if !has_complete_date || !parser.consume(b'T') {
        return None;
    }
    fields[3] = parser.digits(2)? as f64;
    if !parser.consume(b':') {
        return None;
    }
    fields[4] = parser.digits(2)? as f64;
    if parser.consume(b':') {
        fields[5] = parser.digits(2)? as f64;
        if parser.consume(b'.') {
            fields[6] = parser.milliseconds()? as f64;
        }
    }
    if fields[3] > 24.0
        || fields[4] > 59.0
        || fields[5] > 59.0
        || (fields[3] == 24.0 && fields[4..].iter().any(|field| *field != 0.0))
    {
        return None;
    }

    if parser.at_end() {
        return Some(ParsedDateTime::Local(fields));
    }
    let offset_minutes = if parser.consume(b'Z') {
        0_i32
    } else {
        let sign = if parser.consume(b'+') {
            1_i32
        } else if parser.consume(b'-') {
            -1_i32
        } else {
            return None;
        };
        let hours = parser.digits(2)?;
        if !parser.consume(b':') {
            return None;
        }
        let minutes = parser.digits(2)?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        sign * i32::try_from(hours * 60 + minutes).ok()?
    };
    if !parser.at_end() {
        return None;
    }
    let utc = make_utc_date_unclipped(fields) - f64::from(offset_minutes) * MS_PER_MINUTE as f64;
    Some(ParsedDateTime::Utc(time_clip(utc)))
}

/// Parses strings emitted by this engine's toString and toUTCString implementations.
fn parse_engine_date_string(units: &[u16]) -> Option<ParsedDateTime> {
    let mut parser = LegacyDateParser { units, cursor: 0 };
    parser.weekday()?;
    let utc_style = parser.consume(b',');
    parser.expect(b' ')?;
    let (month, day) = if utc_style {
        let day = parser.digits(2)?;
        parser.expect(b' ')?;
        let month = parser.month()?;
        (month, day)
    } else {
        let month = parser.month()?;
        parser.expect(b' ')?;
        let day = parser.digits(2)?;
        (month, day)
    };
    parser.expect(b' ')?;
    let year = parser.year()?;
    parser.expect(b' ')?;
    let hours = parser.digits(2)?;
    parser.expect(b':')?;
    let minutes = parser.digits(2)?;
    parser.expect(b':')?;
    let seconds = parser.digits(2)?;
    parser.expect(b' ')?;
    parser.literal(b"GMT")?;
    let offset_minutes = if utc_style {
        0_i32
    } else {
        let sign = if parser.consume(b'+') {
            1_i32
        } else if parser.consume(b'-') {
            -1_i32
        } else {
            return None;
        };
        let offset_hours = parser.digits(2)?;
        let offset_minutes = parser.digits(2)?;
        if offset_hours > 24 || offset_minutes > 59 {
            return None;
        }
        sign * i32::try_from(offset_hours * 60 + offset_minutes).ok()?
    };
    if !parser.at_end()
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(i32::try_from(year).ok()?, month)
        || hours > 23
        || minutes > 59
        || seconds > 59
    {
        return None;
    }
    let fields = [
        year as f64,
        (month - 1) as f64,
        day as f64,
        hours as f64,
        minutes as f64,
        seconds as f64,
        0.0,
    ];
    let utc = make_utc_date_unclipped(fields) - f64::from(offset_minutes) * MS_PER_MINUTE as f64;
    Some(ParsedDateTime::Utc(time_clip(utc)))
}

struct LegacyDateParser<'a> {
    units: &'a [u16],
    cursor: usize,
}

impl LegacyDateParser<'_> {
    fn at_end(&self) -> bool {
        self.cursor == self.units.len()
    }

    fn consume(&mut self, byte: u8) -> bool {
        if self.units.get(self.cursor).copied() == Some(u16::from(byte)) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Option<()> {
        self.consume(byte).then_some(())
    }

    fn literal(&mut self, bytes: &[u8]) -> Option<()> {
        for &byte in bytes {
            self.expect(byte)?;
        }
        Some(())
    }

    fn digits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0_u32;
        for _ in 0..count {
            let unit = *self.units.get(self.cursor)?;
            let digit = unit.checked_sub(u16::from(b'0'))?;
            if digit > 9 {
                return None;
            }
            value = value.checked_mul(10)?.checked_add(u32::from(digit))?;
            self.cursor += 1;
        }
        Some(value)
    }

    /// Reads a signed, space-terminated year with at least four decimal digits.
    fn year(&mut self) -> Option<i64> {
        let negative = self.consume(b'-');
        let start = self.cursor;
        let mut value = 0_i64;
        while let Some(unit) = self.units.get(self.cursor).copied() {
            let Some(digit) = unit.checked_sub(u16::from(b'0')) else {
                break;
            };
            if digit > 9 {
                break;
            }
            value = value.checked_mul(10)?.checked_add(i64::from(digit))?;
            self.cursor += 1;
        }
        if self.cursor - start < 4 {
            return None;
        }
        Some(if negative { -value } else { value })
    }

    fn weekday(&mut self) -> Option<()> {
        let start = self.cursor;
        self.cursor = self.cursor.checked_add(3)?;
        let candidate = self.units.get(start..self.cursor)?;
        WEEKDAY_NAMES
            .iter()
            .any(|name| ascii_equals(candidate, name))
            .then_some(())
    }

    fn month(&mut self) -> Option<u32> {
        let start = self.cursor;
        self.cursor = self.cursor.checked_add(3)?;
        let candidate = self.units.get(start..self.cursor)?;
        MONTH_NAMES
            .iter()
            .position(|name| ascii_equals(candidate, name))
            .and_then(|index| u32::try_from(index + 1).ok())
    }
}

fn ascii_equals(units: &[u16], bytes: &[u8]) -> bool {
    units.len() == bytes.len()
        && units
            .iter()
            .zip(bytes)
            .all(|(&unit, &byte)| unit == u16::from(byte))
}

struct IsoParser<'a> {
    units: &'a [u16],
    cursor: usize,
}

impl IsoParser<'_> {
    fn year(&mut self) -> Option<i32> {
        let sign = if self.consume(b'+') {
            Some(1_i32)
        } else if self.consume(b'-') {
            Some(-1_i32)
        } else {
            None
        };
        let magnitude = self.digits(if sign.is_some() { 6 } else { 4 })?;
        if sign == Some(-1) && magnitude == 0 {
            return None;
        }
        i32::try_from(magnitude)
            .ok()
            .map(|year| sign.unwrap_or(1) * year)
    }

    fn digits(&mut self, count: usize) -> Option<u32> {
        let end = self.cursor.checked_add(count)?;
        let digits = self.units.get(self.cursor..end)?;
        let mut value = 0_u32;
        for &unit in digits {
            let digit = unit.checked_sub(u16::from(b'0'))?;
            if digit > 9 {
                return None;
            }
            value = value.checked_mul(10)?.checked_add(u32::from(digit))?;
        }
        self.cursor = end;
        Some(value)
    }

    fn milliseconds(&mut self) -> Option<u32> {
        let start = self.cursor;
        let mut value = 0_u32;
        let mut count = 0_usize;
        while let Some(&unit) = self.units.get(self.cursor) {
            let Some(digit) = unit.checked_sub(u16::from(b'0')) else {
                break;
            };
            if digit > 9 {
                break;
            }
            if count < 3 {
                value = value * 10 + u32::from(digit);
            }
            count += 1;
            self.cursor += 1;
        }
        if self.cursor == start {
            return None;
        }
        Some(value * 10_u32.pow(3_u32.saturating_sub(count.min(3) as u32)))
    }

    #[inline]
    fn consume(&mut self, byte: u8) -> bool {
        if self.units.get(self.cursor) == Some(&u16::from(byte)) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    #[inline]
    fn at_end(&self) -> bool {
        self.cursor == self.units.len()
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if is_leap_year(i64::from(year)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_parser_covers_offsets_boundaries_and_local_classification() {
        let parse = |text: &str| parse_iso_date_time(&text.encode_utf16().collect::<Vec<_>>());
        assert_eq!(parse("1970-01-01"), Some(ParsedDateTime::Utc(0.0)));
        assert_eq!(
            parse("1970-01-01T01:30:00+01:30"),
            Some(ParsedDateTime::Utc(0.0))
        );
        assert_eq!(
            parse("1970-01-01T00:00:00"),
            Some(ParsedDateTime::Local([
                1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0
            ]))
        );
        assert!(parse("-000000-01-01T00:00Z").is_none());
        assert!(parse("2023-02-29").is_none());
        assert!(parse("1970-01-01T24:00:00.001Z").is_none());
    }

    #[test]
    fn engine_date_string_parser_round_trips_utc_local_and_signed_years() {
        let parse = |text: &str| parse_date_time(&text.encode_utf16().collect::<Vec<_>>());
        assert_eq!(
            parse("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(ParsedDateTime::Utc(0.0))
        );
        assert_eq!(
            parse("Thu Jan 01 1970 01:30:00 GMT+0130"),
            Some(ParsedDateTime::Utc(0.0))
        );
        assert_eq!(
            parse("Thu, 01 Jul -0001 00:00:00 GMT"),
            Some(ParsedDateTime::Utc(make_utc_date([
                -1.0, 6.0, 1.0, 0.0, 0.0, 0.0, 0.0
            ])))
        );
        assert!(parse("Thu Jan 01 1970 00:00:00 UTC+0000").is_none());
    }
}
