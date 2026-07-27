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
    pub(crate) named_captures: Vec<(Box<str>, Option<core::ops::Range<usize>>)>,
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
        Regex::with_flags(pattern, flags)
            .map(|regex| Self { regex })
            .map_err(|error| error.to_string())
    }

    /// Compiles exact ECMAScript UTF-16 pattern units, including lone surrogates.
    pub(crate) fn compile_units_with_flags(pattern: &[u16], flags: &str) -> Result<Self, String> {
        let full_unicode = flags.bytes().any(|flag| matches!(flag, b'u' | b'v'));
        Regex::from_unicode(
            PatternCodePoints {
                units: pattern,
                index: 0,
                full_unicode,
            },
            flags,
        )
        .map(|regex| Self { regex })
        .map_err(|error| error.to_string())
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
                .map(|(name, range)| (Box::from(name), range))
                .collect(),
        })
    }
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
}
