//! JSON lexical parsing primitives shared by the future parse and stringify builtins.

#![allow(
    dead_code,
    reason = "the native JSON.parse dispatch lands with value materialization"
)]

/// Validates JSON's token stream over UTF-16 code units without accepting JavaScript extensions.
pub(super) fn validate_json(units: &[u16]) -> bool {
    let mut index = 0;
    skip_space(units, &mut index);
    let valid = parse_value(units, &mut index, 0);
    skip_space(units, &mut index);
    valid && index == units.len()
}

fn parse_value(units: &[u16], index: &mut usize, depth: u32) -> bool {
    if depth > 256 {
        return false;
    }
    skip_space(units, index);
    match units.get(*index).copied() {
        Some(110) => consume_literal(units, index, b"null"),
        Some(102) => consume_literal(units, index, b"false"),
        Some(116) => consume_literal(units, index, b"true"),
        Some(34) => parse_string(units, index),
        Some(45 | 48..=57) => parse_number(units, index),
        Some(91) => parse_array(units, index, depth + 1),
        Some(123) => parse_object(units, index, depth + 1),
        _ => false,
    }
}

fn parse_array(units: &[u16], index: &mut usize, depth: u32) -> bool {
    *index += 1;
    skip_space(units, index);
    if consume(units, index, b']') {
        return true;
    }
    loop {
        if !parse_value(units, index, depth) {
            return false;
        }
        skip_space(units, index);
        if consume(units, index, b']') {
            return true;
        }
        if !consume(units, index, b',') {
            return false;
        }
    }
}

fn parse_object(units: &[u16], index: &mut usize, depth: u32) -> bool {
    *index += 1;
    skip_space(units, index);
    if consume(units, index, b'}') {
        return true;
    }
    loop {
        if !parse_string(units, index) {
            return false;
        }
        skip_space(units, index);
        if !consume(units, index, b':') || !parse_value(units, index, depth) {
            return false;
        }
        skip_space(units, index);
        if consume(units, index, b'}') {
            return true;
        }
        if !consume(units, index, b',') {
            return false;
        }
        skip_space(units, index);
    }
}

fn parse_string(units: &[u16], index: &mut usize) -> bool {
    if !consume(units, index, b'"') {
        return false;
    }
    while let Some(unit) = units.get(*index).copied() {
        *index += 1;
        match unit {
            34 => return true,
            0..=0x1f => return false,
            92 => match units.get(*index).copied() {
                Some(34 | 47 | 92 | 98 | 102 | 110 | 114 | 116) => *index += 1,
                Some(117)
                    if *index + 5 <= units.len()
                        && units[*index + 1..*index + 5]
                            .iter()
                            .all(|unit| is_hex(*unit)) =>
                {
                    *index += 5
                }
                _ => return false,
            },
            _ => {}
        }
    }
    false
}

fn parse_number(units: &[u16], index: &mut usize) -> bool {
    consume(units, index, b'-');
    match units.get(*index).copied() {
        Some(48) => *index += 1,
        Some(49..=57) => {
            while units.get(*index).is_some_and(|unit| is_digit(*unit)) {
                *index += 1;
            }
        }
        _ => return false,
    }
    if consume(units, index, b'.') && !consume_digits(units, index) {
        return false;
    }
    if matches!(units.get(*index), Some(69 | 101)) {
        *index += 1;
        let _ = consume(units, index, b'+') || consume(units, index, b'-');
        if !consume_digits(units, index) {
            return false;
        }
    }
    true
}

fn consume_literal(units: &[u16], index: &mut usize, literal: &[u8]) -> bool {
    if units
        .get(*index..*index + literal.len())
        .is_some_and(|candidate| {
            candidate
                .iter()
                .copied()
                .eq(literal.iter().copied().map(u16::from))
        })
    {
        *index += literal.len();
        true
    } else {
        false
    }
}
fn consume_digits(units: &[u16], index: &mut usize) -> bool {
    let start = *index;
    while units.get(*index).is_some_and(|unit| is_digit(*unit)) {
        *index += 1;
    }
    *index != start
}
fn consume(units: &[u16], index: &mut usize, expected: u8) -> bool {
    if units.get(*index) == Some(&u16::from(expected)) {
        *index += 1;
        true
    } else {
        false
    }
}
fn skip_space(units: &[u16], index: &mut usize) {
    while matches!(units.get(*index), Some(0x20 | 0x09 | 0x0a | 0x0d)) {
        *index += 1;
    }
}

const fn is_digit(unit: u16) -> bool {
    unit >= 48 && unit <= 57
}
const fn is_hex(unit: u16) -> bool {
    is_digit(unit) || (unit >= 65 && unit <= 70) || (unit >= 97 && unit <= 102)
}

#[cfg(test)]
mod tests {
    use super::validate_json;
    #[test]
    fn grammar_rejects_javascript_extensions() {
        assert!(validate_json(
            "{\"a\":[null,true,-1.2e+3]}"
                .encode_utf16()
                .collect::<Vec<_>>()
                .as_slice()
        ));
        assert!(!validate_json(
            b"{'a':1}"
                .iter()
                .copied()
                .map(u16::from)
                .collect::<Vec<_>>()
                .as_slice()
        ));
        assert!(!validate_json(
            b"[1,]"
                .iter()
                .copied()
                .map(u16::from)
                .collect::<Vec<_>>()
                .as_slice()
        ));
    }
}
