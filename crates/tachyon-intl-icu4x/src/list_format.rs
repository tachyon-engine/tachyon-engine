//! ICU4X compiled-data implementation of the provider-neutral ListFormat ABI.

use core::fmt::{self, Write};

use icu_list::{
    ListFormatter, ListFormatterPreferences,
    options::{ListFormatterOptions, ListLength},
};
use icu_locale::Locale;
use tachyon_vm::{
    HostProviderError, IntlFormattedListParts, IntlListFormatPartSpan, IntlListFormatPartType,
    IntlListFormatRequest, IntlListFormatResolved, IntlListFormatStyle, IntlListFormatType,
    IntlLocaleMatcher,
};
use writeable::{Part, PartsWrite, Writeable};

const DATA_FAILURE: HostProviderError = HostProviderError::Failure(6);

/// Resolves the first locale with compiled list-pattern data and freezes scalar slots.
pub(super) fn create(
    default_locale: &str,
    request: IntlListFormatRequest,
) -> Result<IntlListFormatResolved, HostProviderError> {
    let locale = request
        .locales
        .iter()
        .find_map(|locale| resolve_locale(locale, request.list_type, request.style))
        .or_else(|| resolve_locale(default_locale, request.list_type, request.style))
        .ok_or(DATA_FAILURE)?;
    Ok(IntlListFormatResolved {
        locale: locale.to_string().into_boxed_str(),
        list_type: request.list_type,
        style: request.style,
    })
}

/// Formats UTF-16 elements and records ICU's element/literal boundaries without reparsing output.
pub(super) fn format(
    resolved: &IntlListFormatResolved,
    elements: &[Box<[u16]>],
) -> Result<IntlFormattedListParts, HostProviderError> {
    let locale = resolved
        .locale
        .parse::<Locale>()
        .map_err(|_| DATA_FAILURE)?;
    let formatter = formatter(&locale, resolved.list_type, resolved.style)?;
    let placeholders = vec!["x"; elements.len()];
    let mut collector = Utf16PartsCollector::new(elements, elements.len().saturating_mul(2));
    formatter
        .format(placeholders.into_iter())
        .write_to_parts(&mut collector)
        .map_err(|_| DATA_FAILURE)?;
    collector.finish()
}

/// Returns only requested spellings whose base locale has compiled list data.
pub(super) fn supported_locales(
    locales: &[Box<str>],
    _matcher: IntlLocaleMatcher,
) -> Box<[Box<str>]> {
    locales
        .iter()
        .filter(|locale| {
            resolve_locale(
                locale,
                IntlListFormatType::Conjunction,
                IntlListFormatStyle::Long,
            )
            .is_some()
        })
        .cloned()
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn resolve_locale(
    locale: &str,
    list_type: IntlListFormatType,
    style: IntlListFormatStyle,
) -> Option<Locale> {
    let mut locale = locale.parse::<Locale>().ok()?;
    locale.extensions = Default::default();
    if matches!(locale.id.language.as_str(), "und" | "zxx") {
        return None;
    }
    formatter(&locale, list_type, style).ok()?;
    Some(locale)
}

fn formatter(
    locale: &Locale,
    list_type: IntlListFormatType,
    style: IntlListFormatStyle,
) -> Result<ListFormatter, HostProviderError> {
    let preferences = ListFormatterPreferences::from(locale);
    let options = ListFormatterOptions::default().with_length(match style {
        IntlListFormatStyle::Long => ListLength::Wide,
        IntlListFormatStyle::Short => ListLength::Short,
        IntlListFormatStyle::Narrow => ListLength::Narrow,
    });
    match list_type {
        IntlListFormatType::Conjunction => ListFormatter::try_new_and(preferences, options),
        IntlListFormatType::Disjunction => ListFormatter::try_new_or(preferences, options),
        IntlListFormatType::Unit => ListFormatter::try_new_unit(preferences, options),
    }
    .map_err(|_| DATA_FAILURE)
}

struct Utf16PartsCollector<'a> {
    elements: &'a [Box<[u16]>],
    element_index: usize,
    formatted: Vec<u16>,
    spans: Vec<IntlListFormatPartSpan>,
}

impl<'a> Utf16PartsCollector<'a> {
    fn new(elements: &'a [Box<[u16]>], span_capacity: usize) -> Self {
        Self {
            elements,
            element_index: 0,
            formatted: Vec::new(),
            spans: Vec::with_capacity(span_capacity),
        }
    }

    fn finish(self) -> Result<IntlFormattedListParts, HostProviderError> {
        if self.element_index != self.elements.len() {
            return Err(DATA_FAILURE);
        }
        validate_spans(&self.spans, self.formatted.len())?;
        Ok(IntlFormattedListParts {
            formatted: self.formatted.into_boxed_slice(),
            spans: self.spans.into_boxed_slice(),
        })
    }
}

impl Write for Utf16PartsCollector<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.formatted.extend(value.encode_utf16());
        Ok(())
    }
}

impl PartsWrite for Utf16PartsCollector<'_> {
    type SubPartsWrite = StringPart;

    fn with_part(
        &mut self,
        part: Part,
        mut write: impl FnMut(&mut Self::SubPartsWrite) -> fmt::Result,
    ) -> fmt::Result {
        let kind = match (part.category, part.value) {
            ("list", "element") => IntlListFormatPartType::Element,
            ("list", "literal") => IntlListFormatPartType::Literal,
            _ => return Err(fmt::Error),
        };
        let mut value = StringPart(String::new());
        write(&mut value)?;
        let start = u32::try_from(self.formatted.len()).map_err(|_| fmt::Error)?;
        match kind {
            IntlListFormatPartType::Element => {
                let element = self.elements.get(self.element_index).ok_or(fmt::Error)?;
                self.formatted.extend_from_slice(element);
                self.element_index = self.element_index.checked_add(1).ok_or(fmt::Error)?;
            }
            IntlListFormatPartType::Literal => self.formatted.extend(value.0.encode_utf16()),
        }
        let end = u32::try_from(self.formatted.len()).map_err(|_| fmt::Error)?;
        if start == end && kind == IntlListFormatPartType::Literal {
            return Ok(());
        }
        self.spans.push(IntlListFormatPartSpan { kind, start, end });
        Ok(())
    }
}

struct StringPart(String);

impl Write for StringPart {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0.push_str(value);
        Ok(())
    }
}

impl PartsWrite for StringPart {
    type SubPartsWrite = Self;

    fn with_part(
        &mut self,
        _part: Part,
        mut write: impl FnMut(&mut Self::SubPartsWrite) -> fmt::Result,
    ) -> fmt::Result {
        write(self)
    }
}

fn validate_spans(
    spans: &[IntlListFormatPartSpan],
    formatted_len: usize,
) -> Result<(), HostProviderError> {
    let mut cursor = 0_u32;
    for span in spans {
        if span.start != cursor || span.end < span.start {
            return Err(DATA_FAILURE);
        }
        cursor = span.end;
    }
    if usize::try_from(cursor).ok() != Some(formatted_len) {
        return Err(DATA_FAILURE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_english_conjunction_with_gap_free_parts() {
        let resolved = create(
            "en-US",
            IntlListFormatRequest {
                locales: vec![Box::<str>::from("en-US")].into_boxed_slice(),
                ..Default::default()
            },
        )
        .unwrap();
        let elements = utf16_elements(&["foo", "bar", "baz"]);
        let formatted = format(&resolved, &elements).unwrap();
        assert_eq!(
            String::from_utf16_lossy(&formatted.formatted),
            "foo, bar, and baz"
        );
        assert_eq!(formatted.spans.len(), 5);
        assert_eq!(formatted.spans[0].kind, IntlListFormatPartType::Element);
        assert_eq!(formatted.spans[1].kind, IntlListFormatPartType::Literal);
        assert_eq!(formatted.spans[4].end as usize, formatted.formatted.len());
    }

    #[test]
    fn honors_disjunction_unit_and_width_patterns() {
        let cases = [
            (
                "en-US",
                IntlListFormatType::Disjunction,
                IntlListFormatStyle::Long,
                "foo, bar, or baz",
            ),
            (
                "en-US",
                IntlListFormatType::Conjunction,
                IntlListFormatStyle::Short,
                "foo, bar, & baz",
            ),
            (
                "es-ES",
                IntlListFormatType::Unit,
                IntlListFormatStyle::Long,
                "foo, bar y baz",
            ),
        ];
        let elements = utf16_elements(&["foo", "bar", "baz"]);
        for (locale, list_type, style, expected) in cases {
            let resolved = create(
                "en-US",
                IntlListFormatRequest {
                    locales: vec![Box::<str>::from(locale)].into_boxed_slice(),
                    list_type,
                    style,
                    ..Default::default()
                },
            )
            .unwrap();
            let formatted = format(&resolved, &elements).unwrap();
            assert_eq!(String::from_utf16_lossy(&formatted.formatted), expected);
        }
    }

    #[test]
    fn preserves_utf16_elements_and_reports_supported_base_locales() {
        let resolved = create(
            "en-US",
            IntlListFormatRequest {
                locales: vec![Box::<str>::from("en-US")].into_boxed_slice(),
                ..Default::default()
            },
        )
        .unwrap();
        let elements =
            vec![Box::<[u16]>::from([0xd800]), Box::<[u16]>::from([])].into_boxed_slice();
        let formatted = format(&resolved, &elements).unwrap();
        assert_eq!(formatted.spans.len(), 3);
        assert_eq!(formatted.spans[0].kind, IntlListFormatPartType::Element);
        assert_eq!(formatted.formatted[0], 0xd800);
        assert_eq!(formatted.spans[2].start, formatted.spans[2].end);

        let requested = vec![Box::<str>::from("en")].into_boxed_slice();
        assert_eq!(
            supported_locales(&requested, IntlLocaleMatcher::BestFit),
            requested
        );
    }

    fn utf16_elements(values: &[&str]) -> Box<[Box<[u16]>]> {
        values
            .iter()
            .map(|value| value.encode_utf16().collect::<Vec<_>>().into_boxed_slice())
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }
}
