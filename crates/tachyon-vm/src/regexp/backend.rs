//! UTF-16 matching adapter over the pinned ECMAScript-pattern compiler.

use regress::Regex;

/// Iterates an ECMAScript pattern as code units or code points according to `u`/`v`.
#[derive(Clone)]
struct PatternCodePoints<'a> {
    units: &'a [u16],
    index: usize,
    full_unicode: bool,
}

impl Iterator for PatternCodePoints<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let first = *self.units.get(self.index)?;
        self.index += 1;
        if self.full_unicode
            && (0xd800..=0xdbff).contains(&first)
            && self
                .units
                .get(self.index)
                .is_some_and(|second| (0xdc00..=0xdfff).contains(second))
        {
            let second = self.units[self.index];
            self.index += 1;
            return Some(
                0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00),
            );
        }
        Some(u32::from(first))
    }
}

/// Immutable compiled pattern kept behind the VM's RegExp object layer.
#[allow(
    dead_code,
    reason = "RegExp object integration follows the backend contract"
)]
pub(crate) struct CompiledRegExp {
    regex: Regex,
    named_capture_indices: Vec<(Box<str>, usize)>,
}

/// One named capture paired with its exact numeric capture slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegExpNamedCapture {
    pub(crate) name: Box<str>,
    pub(crate) capture_index: usize,
    pub(crate) range: Option<core::ops::Range<usize>>,
}

/// A successful match expressed only in ECMAScript UTF-16 code-unit offsets.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "RegExp object integration follows the backend contract"
)]
pub(crate) struct RegExpMatch {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) captures: Vec<Option<core::ops::Range<usize>>>,
    pub(crate) named_captures: Vec<RegExpNamedCapture>,
}

#[allow(
    dead_code,
    reason = "RegExp object integration follows the backend contract"
)]
impl CompiledRegExp {
    /// Compiles one already-validated ECMAScript pattern without exposing the backend API.
    pub(crate) fn compile(pattern: &str) -> Result<Self, String> {
        Self::compile_with_flags(pattern, "")
    }

    /// Compiles a pattern with ECMAScript flags after the VM has validated duplicate handling.
    pub(crate) fn compile_with_flags(pattern: &str, flags: &str) -> Result<Self, String> {
        let units = pattern.encode_utf16().collect::<Vec<_>>();
        Self::compile_units_with_flags(&units, flags)
    }

    /// Compiles exact ECMAScript UTF-16 pattern units, including lone surrogates.
    pub(crate) fn compile_units_with_flags(pattern: &[u16], flags: &str) -> Result<Self, String> {
        let full_unicode = flags.bytes().any(|flag| matches!(flag, b'u' | b'v'));
        let regex = Regex::from_unicode(
            PatternCodePoints {
                units: pattern,
                index: 0,
                full_unicode,
            },
            flags,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            regex,
            named_capture_indices: regexp_named_capture_indices(pattern, flags.contains('v'))?,
        })
    }

    /// Finds the first UCS-2 match at or after `start`, preserving code-unit positions.
    pub(crate) fn find_ucs2(&self, input: &[u16], start: usize) -> Option<RegExpMatch> {
        self.find(input, start, false)
    }

    /// Selects code-unit or code-point traversal while preserving code-unit offsets.
    pub(crate) fn find(
        &self,
        input: &[u16],
        start: usize,
        full_unicode: bool,
    ) -> Option<RegExpMatch> {
        let matched = if full_unicode {
            self.regex.find_from_utf16(input, start).next()
        } else {
            self.regex.find_from_ucs2(input, start).next()
        }?;
        let range = matched.range();
        Some(RegExpMatch {
            start: range.start,
            end: range.end,
            captures: matched.groups().skip(1).collect(),
            named_captures: matched
                .named_groups()
                .map(|(name, range)| {
                    let capture_index = self
                        .named_capture_indices
                        .iter()
                        .filter(|(candidate, _)| candidate.as_ref() == name)
                        .find_map(|(_, index)| {
                            (matched.captures.get(*index) == Some(&range)).then_some(*index)
                        })?;
                    Some(RegExpNamedCapture {
                        name: Box::from(name),
                        capture_index,
                        range,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        })
    }
}

/// Extracts decoded named-group indices while respecting escapes and nested `v` classes.
fn regexp_named_capture_indices(
    pattern: &[u16],
    unicode_sets: bool,
) -> Result<Vec<(Box<str>, usize)>, String> {
    let mut captures = Vec::new();
    let mut capture_index = 0;
    let mut class_depth = 0_u32;
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            unit if unit == u16::from(b'\\') => index = index.saturating_add(2),
            unit if unit == u16::from(b'[') && (class_depth == 0 || unicode_sets) => {
                class_depth = class_depth.saturating_add(1);
                index += 1;
            }
            unit if unit == u16::from(b']') && class_depth != 0 => {
                class_depth -= 1;
                index += 1;
            }
            unit if unit == u16::from(b'(') && class_depth == 0 => {
                let question = pattern.get(index + 1) == Some(&u16::from(b'?'));
                let named = question
                    && pattern.get(index + 2) == Some(&u16::from(b'<'))
                    && !matches!(pattern.get(index + 3), Some(unit) if *unit == u16::from(b'=') || *unit == u16::from(b'!'));
                if !question || named {
                    let current = capture_index;
                    capture_index += 1;
                    if named {
                        let name_start = index + 3;
                        let name_end = pattern[name_start..]
                            .iter()
                            .position(|unit| *unit == u16::from(b'>'))
                            .map(|offset| name_start + offset)
                            .ok_or_else(|| "unterminated named capture".to_owned())?;
                        let name = decode_regexp_group_name(&pattern[name_start..name_end])?;
                        captures.push((name.into_boxed_str(), current));
                    }
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    Ok(captures)
}

/// Decodes the identifier escapes accepted inside one already-validated group name.
fn decode_regexp_group_name(units: &[u16]) -> Result<String, String> {
    let mut decoded = String::new();
    let mut index = 0;
    while index < units.len() {
        if units[index] == u16::from(b'\\') && units.get(index + 1) == Some(&u16::from(b'u')) {
            let (value, consumed) = decode_group_name_unicode_escape(&units[index + 2..])?;
            decoded
                .push(char::from_u32(value).ok_or_else(|| "invalid group name escape".to_owned())?);
            index += consumed + 2;
            continue;
        }
        let first = units[index];
        let consumed = if (0xd800..=0xdbff).contains(&first)
            && units
                .get(index + 1)
                .is_some_and(|second| (0xdc00..=0xdfff).contains(second))
        {
            2
        } else {
            1
        };
        let character = char::decode_utf16(units[index..index + consumed].iter().copied())
            .next()
            .ok_or_else(|| "missing UTF-16 group name character".to_owned())?
            .map_err(|_| "invalid UTF-16 group name".to_owned())?;
        decoded.push(character);
        index += consumed;
    }
    Ok(decoded)
}

/// Parses either `XXXX` or `{X...}` after the `\\u` prefix.
fn decode_group_name_unicode_escape(units: &[u16]) -> Result<(u32, usize), String> {
    if units.first() == Some(&u16::from(b'{')) {
        let end = units
            .iter()
            .position(|unit| *unit == u16::from(b'}'))
            .ok_or_else(|| "unterminated group name escape".to_owned())?;
        let value = decode_hex_units(&units[1..end])?;
        return Ok((value, end + 1));
    }
    let digits = units
        .get(..4)
        .ok_or_else(|| "short group name escape".to_owned())?;
    Ok((decode_hex_units(digits)?, 4))
}

/// Converts a non-empty ASCII hexadecimal slice without allocation.
fn decode_hex_units(units: &[u16]) -> Result<u32, String> {
    if units.is_empty() {
        return Err("empty group name escape".to_owned());
    }
    units.iter().try_fold(0_u32, |value, unit| {
        let digit = char::from_u32(u32::from(*unit))
            .and_then(|character| character.to_digit(16))
            .ok_or_else(|| "invalid group name escape".to_owned())?;
        value
            .checked_mul(16)
            .and_then(|value| value.checked_add(digit))
            .ok_or_else(|| "group name escape overflow".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::CompiledRegExp;

    #[test]
    fn ucs2_matching_reports_ecmascript_code_unit_offsets() {
        let regex = CompiledRegExp::compile("b.").unwrap();
        let matched = regex
            .find_ucs2(&[u16::from(b'a'), u16::from(b'b'), 0xd800], 0)
            .unwrap();
        assert_eq!((matched.start, matched.end), (1, 3));
    }

    #[test]
    fn unicode_matching_combines_surrogate_pairs_but_reports_code_units() {
        let regex = CompiledRegExp::compile_units_with_flags(&[u16::from(b'.')], "u").unwrap();
        let matched = regex.find(&[0xd834, 0xdf06], 0, true).unwrap();
        assert_eq!((matched.start, matched.end), (0, 2));
    }

    #[test]
    fn unicode_property_and_sets_flags_use_the_pinned_backend() {
        let units = "\\p{Script=Han}".encode_utf16().collect::<Vec<_>>();
        for flags in ["u", "v"] {
            let regex = CompiledRegExp::compile_units_with_flags(&units, flags).unwrap();
            let input = "𠮷".encode_utf16().collect::<Vec<_>>();
            let matched = regex.find(&input, 0, true).unwrap();
            assert_eq!((matched.start, matched.end), (0, 2));
        }
    }

    #[test]
    fn named_capture_metadata_distinguishes_equal_nested_ranges() {
        let units = r"((?<same>a))".encode_utf16().collect::<Vec<_>>();
        let regex = CompiledRegExp::compile_units_with_flags(&units, "").unwrap();
        let matched = regex.find_ucs2(&[u16::from(b'a')], 0).unwrap();
        assert_eq!(matched.named_captures[0].capture_index, 1);
        assert_eq!(matched.named_captures[0].range, Some(0..1));
    }

    #[test]
    fn named_capture_metadata_decodes_identifier_escapes() {
        let units = r"(?<\u0061>a)".encode_utf16().collect::<Vec<_>>();
        let regex = CompiledRegExp::compile_units_with_flags(&units, "").unwrap();
        let matched = regex.find_ucs2(&[u16::from(b'a')], 0).unwrap();
        assert_eq!(matched.named_captures[0].name.as_ref(), "a");
        assert_eq!(matched.named_captures[0].capture_index, 0);
    }
}
