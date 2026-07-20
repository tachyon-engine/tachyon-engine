//! UTF-16 matching adapter over the pinned ECMAScript-pattern compiler.

use regress::Regex;

/// Immutable compiled pattern kept behind the VM's RegExp object layer.
#[allow(
    dead_code,
    reason = "RegExp object integration follows the backend contract"
)]
pub(crate) struct CompiledRegExp {
    regex: Regex,
}

/// A successful match expressed only in ECMAScript UTF-16 code-unit offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "RegExp object integration follows the backend contract"
)]
pub(crate) struct RegExpMatch {
    pub(crate) start: usize,
    pub(crate) end: usize,
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

    /// Finds the first UCS-2 match at or after `start`, preserving code-unit positions.
    pub(crate) fn find_ucs2(&self, input: &[u16], start: usize) -> Option<RegExpMatch> {
        self.regex
            .find_from_ucs2(input, start)
            .next()
            .map(|matched| {
                let range = matched.range();
                RegExpMatch {
                    start: range.start,
                    end: range.end,
                }
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
}
