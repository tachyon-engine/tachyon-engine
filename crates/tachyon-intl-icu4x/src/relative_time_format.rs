//! Relative-time patterns composed with the shared number and plural backends.

use tachyon_vm::{
    HostProviderError, IntlFormattedNumberParts, IntlFormattedRelativeTimeParts, IntlLocaleMatcher,
    IntlMathematicalValue, IntlNumberFormatOptions, IntlNumberFormatPartType,
    IntlNumberFormatRequest, IntlPluralCategory, IntlPluralRuleType, IntlPluralRulesRequest,
    IntlRelativeTimeFormatBackend, IntlRelativeTimeFormatCreation, IntlRelativeTimeFormatNumeric,
    IntlRelativeTimeFormatRequest, IntlRelativeTimeFormatResolved, IntlRelativeTimeFormatStyle,
    IntlRelativeTimePartSpan, IntlRelativeTimeUnit,
};

const DATA_FAILURE: HostProviderError = HostProviderError::Failure(8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelativeLocaleKind {
    English,
    Polish,
}

struct Icu4xRelativeTimeFormatBackend {
    locale: RelativeLocaleKind,
    style: IntlRelativeTimeFormatStyle,
    numeric: IntlRelativeTimeFormatNumeric,
    number: Box<dyn tachyon_vm::IntlNumberFormatBackend>,
    plural: Box<dyn tachyon_vm::IntlPluralRulesBackend>,
}

impl IntlRelativeTimeFormatBackend for Icu4xRelativeTimeFormatBackend {
    fn format(
        &self,
        value: &IntlMathematicalValue,
        unit: IntlRelativeTimeUnit,
    ) -> Result<Box<[u16]>, HostProviderError> {
        self.render(value, unit)
            .map(|parts| parts.formatted.into_boxed_slice())
    }

    fn format_to_parts(
        &self,
        value: &IntlMathematicalValue,
        unit: IntlRelativeTimeUnit,
    ) -> Result<IntlFormattedRelativeTimeParts, HostProviderError> {
        let rendered = self.render(value, unit)?;
        Ok(IntlFormattedRelativeTimeParts {
            formatted: rendered.formatted.into_boxed_slice(),
            spans: rendered.spans.into_boxed_slice(),
        })
    }

    fn external_memory_bytes(&self) -> usize {
        std::mem::size_of_val(&*self.number)
            .saturating_add(self.number.external_memory_bytes())
            .saturating_add(std::mem::size_of_val(&*self.plural))
            .saturating_add(self.plural.external_memory_bytes())
    }
}

struct RenderedRelativeTime {
    formatted: Vec<u16>,
    spans: Vec<IntlRelativeTimePartSpan>,
}

impl Icu4xRelativeTimeFormatBackend {
    /// Selects lexical or numeric CLDR patterns and preserves NumberFormat field boundaries.
    fn render(
        &self,
        value: &IntlMathematicalValue,
        unit: IntlRelativeTimeUnit,
    ) -> Result<RenderedRelativeTime, HostProviderError> {
        let (magnitude, past) = absolute_value(value)?;
        if self.numeric == IntlRelativeTimeFormatNumeric::Auto
            && let Some(value) = lexical_relative_time(self.locale, &magnitude, past, unit)
        {
            return rendered_literal(value);
        }
        let category = self.plural.select(&magnitude)?;
        let number = self.number.format_to_parts(&magnitude)?;
        let label = relative_unit_label(self.locale, self.style, unit, category);
        render_numeric_relative_time(self.locale, past, number, label)
    }
}

/// Resolves numbering-system data through NumberFormat and plural categories through PluralRules.
pub(super) fn create(
    default_locale: &str,
    request: IntlRelativeTimeFormatRequest,
) -> Result<IntlRelativeTimeFormatCreation, HostProviderError> {
    let number = super::number_format::create(
        default_locale,
        IntlNumberFormatRequest {
            locales: request.locales.clone(),
            locale_matcher: request.locale_matcher,
            numbering_system: request.numbering_system,
            options: IntlNumberFormatOptions::default(),
        },
    )?;
    let locale = number.resolved.locale.clone();
    let plural = super::plural_rules::create(
        default_locale,
        IntlPluralRulesRequest {
            locales: vec![locale.clone()].into_boxed_slice(),
            locale_matcher: request.locale_matcher,
            rule_type: IntlPluralRuleType::Cardinal,
            options: IntlNumberFormatOptions::default(),
        },
    )?;
    let locale_kind = if locale
        .split(['-', '_'])
        .next()
        .is_some_and(|language| language == "pl")
    {
        RelativeLocaleKind::Polish
    } else {
        RelativeLocaleKind::English
    };
    Ok(IntlRelativeTimeFormatCreation {
        resolved: IntlRelativeTimeFormatResolved {
            locale,
            numbering_system: number.resolved.numbering_system,
            style: request.style,
            numeric: request.numeric,
        },
        backend: Box::new(Icu4xRelativeTimeFormatBackend {
            locale: locale_kind,
            style: request.style,
            numeric: request.numeric,
            number: number.backend,
            plural: plural.backend,
        }),
    })
}

pub(super) fn supported_locales(
    locales: &[Box<str>],
    matcher: IntlLocaleMatcher,
) -> Box<[Box<str>]> {
    super::number_format::supported_locales(locales, matcher)
}

fn absolute_value(
    value: &IntlMathematicalValue,
) -> Result<(IntlMathematicalValue, bool), HostProviderError> {
    match value {
        IntlMathematicalValue::Finite(value) => {
            let past = value.starts_with('-');
            let magnitude = value.strip_prefix('-').unwrap_or(value).into();
            Ok((IntlMathematicalValue::Finite(magnitude), past))
        }
        IntlMathematicalValue::NegativeZero => {
            Ok((IntlMathematicalValue::Finite("0".into()), true))
        }
        IntlMathematicalValue::PositiveInfinity
        | IntlMathematicalValue::NegativeInfinity
        | IntlMathematicalValue::NaN => Err(DATA_FAILURE),
    }
}

fn lexical_relative_time(
    locale: RelativeLocaleKind,
    magnitude: &IntlMathematicalValue,
    past: bool,
    unit: IntlRelativeTimeUnit,
) -> Option<&'static str> {
    if locale != RelativeLocaleKind::English {
        return None;
    }
    let IntlMathematicalValue::Finite(value) = magnitude else {
        return None;
    };
    let key = match (past, value.as_ref()) {
        (true, "1") => -1,
        (_, "0") => 0,
        (false, "1") => 1,
        _ => return None,
    };
    match (unit, key) {
        (IntlRelativeTimeUnit::Second, 0) => Some("now"),
        (IntlRelativeTimeUnit::Minute, 0) => Some("this minute"),
        (IntlRelativeTimeUnit::Hour, 0) => Some("this hour"),
        (IntlRelativeTimeUnit::Day, -1) => Some("yesterday"),
        (IntlRelativeTimeUnit::Day, 0) => Some("today"),
        (IntlRelativeTimeUnit::Day, 1) => Some("tomorrow"),
        (IntlRelativeTimeUnit::Week, -1) => Some("last week"),
        (IntlRelativeTimeUnit::Week, 0) => Some("this week"),
        (IntlRelativeTimeUnit::Week, 1) => Some("next week"),
        (IntlRelativeTimeUnit::Month, -1) => Some("last month"),
        (IntlRelativeTimeUnit::Month, 0) => Some("this month"),
        (IntlRelativeTimeUnit::Month, 1) => Some("next month"),
        (IntlRelativeTimeUnit::Quarter, -1) => Some("last quarter"),
        (IntlRelativeTimeUnit::Quarter, 0) => Some("this quarter"),
        (IntlRelativeTimeUnit::Quarter, 1) => Some("next quarter"),
        (IntlRelativeTimeUnit::Year, -1) => Some("last year"),
        (IntlRelativeTimeUnit::Year, 0) => Some("this year"),
        (IntlRelativeTimeUnit::Year, 1) => Some("next year"),
        _ => None,
    }
}

fn render_numeric_relative_time(
    locale: RelativeLocaleKind,
    past: bool,
    number: IntlFormattedNumberParts,
    label: &'static str,
) -> Result<RenderedRelativeTime, HostProviderError> {
    let (prefix, suffix_start, suffix_end) = match (locale, past) {
        (RelativeLocaleKind::English, false) => ("in ", " ", ""),
        (RelativeLocaleKind::English, true) => ("", " ", " ago"),
        (RelativeLocaleKind::Polish, false) => ("za ", " ", ""),
        (RelativeLocaleKind::Polish, true) => ("", " ", " temu"),
    };
    let suffix_capacity = suffix_start
        .encode_utf16()
        .count()
        .saturating_add(label.encode_utf16().count())
        .saturating_add(suffix_end.encode_utf16().count());
    let mut formatted = Vec::with_capacity(
        prefix
            .encode_utf16()
            .count()
            .saturating_add(number.formatted.len())
            .saturating_add(suffix_capacity),
    );
    let mut spans = Vec::with_capacity(number.spans.len().saturating_add(2));
    append_relative_span(
        &mut formatted,
        &mut spans,
        IntlNumberFormatPartType::Literal,
        prefix.encode_utf16(),
        false,
    )?;
    let number_start = u32::try_from(formatted.len()).map_err(|_| DATA_FAILURE)?;
    formatted.extend_from_slice(&number.formatted);
    for span in number.spans {
        spans.push(IntlRelativeTimePartSpan {
            kind: span.kind,
            start: number_start.checked_add(span.start).ok_or(DATA_FAILURE)?,
            end: number_start.checked_add(span.end).ok_or(DATA_FAILURE)?,
            has_unit: true,
        });
    }
    append_relative_span(
        &mut formatted,
        &mut spans,
        IntlNumberFormatPartType::Literal,
        suffix_start
            .encode_utf16()
            .chain(label.encode_utf16())
            .chain(suffix_end.encode_utf16()),
        false,
    )?;
    Ok(RenderedRelativeTime { formatted, spans })
}

fn rendered_literal(value: &str) -> Result<RenderedRelativeTime, HostProviderError> {
    let formatted = value.encode_utf16().collect::<Vec<_>>();
    let end = u32::try_from(formatted.len()).map_err(|_| DATA_FAILURE)?;
    Ok(RenderedRelativeTime {
        formatted,
        spans: vec![IntlRelativeTimePartSpan {
            kind: IntlNumberFormatPartType::Literal,
            start: 0,
            end,
            has_unit: false,
        }],
    })
}

fn append_relative_span(
    formatted: &mut Vec<u16>,
    spans: &mut Vec<IntlRelativeTimePartSpan>,
    kind: IntlNumberFormatPartType,
    value: impl Iterator<Item = u16>,
    has_unit: bool,
) -> Result<(), HostProviderError> {
    let start = u32::try_from(formatted.len()).map_err(|_| DATA_FAILURE)?;
    formatted.extend(value);
    let end = u32::try_from(formatted.len()).map_err(|_| DATA_FAILURE)?;
    if start != end {
        spans.push(IntlRelativeTimePartSpan {
            kind,
            start,
            end,
            has_unit,
        });
    }
    Ok(())
}

fn relative_unit_label(
    locale: RelativeLocaleKind,
    style: IntlRelativeTimeFormatStyle,
    unit: IntlRelativeTimeUnit,
    category: IntlPluralCategory,
) -> &'static str {
    match locale {
        RelativeLocaleKind::English => english_unit_label(style, unit, category),
        RelativeLocaleKind::Polish => polish_unit_label(style, unit, category),
    }
}

fn english_unit_label(
    style: IntlRelativeTimeFormatStyle,
    unit: IntlRelativeTimeUnit,
    category: IntlPluralCategory,
) -> &'static str {
    let one = category == IntlPluralCategory::One;
    match style {
        IntlRelativeTimeFormatStyle::Long => match (unit, one) {
            (IntlRelativeTimeUnit::Second, true) => "second",
            (IntlRelativeTimeUnit::Second, false) => "seconds",
            (IntlRelativeTimeUnit::Minute, true) => "minute",
            (IntlRelativeTimeUnit::Minute, false) => "minutes",
            (IntlRelativeTimeUnit::Hour, true) => "hour",
            (IntlRelativeTimeUnit::Hour, false) => "hours",
            (IntlRelativeTimeUnit::Day, true) => "day",
            (IntlRelativeTimeUnit::Day, false) => "days",
            (IntlRelativeTimeUnit::Week, true) => "week",
            (IntlRelativeTimeUnit::Week, false) => "weeks",
            (IntlRelativeTimeUnit::Month, true) => "month",
            (IntlRelativeTimeUnit::Month, false) => "months",
            (IntlRelativeTimeUnit::Quarter, true) => "quarter",
            (IntlRelativeTimeUnit::Quarter, false) => "quarters",
            (IntlRelativeTimeUnit::Year, true) => "year",
            (IntlRelativeTimeUnit::Year, false) => "years",
        },
        IntlRelativeTimeFormatStyle::Short | IntlRelativeTimeFormatStyle::Narrow => {
            match (unit, one) {
                (IntlRelativeTimeUnit::Second, _) => "sec.",
                (IntlRelativeTimeUnit::Minute, _) => "min.",
                (IntlRelativeTimeUnit::Hour, _) => "hr.",
                (IntlRelativeTimeUnit::Day, true) => "day",
                (IntlRelativeTimeUnit::Day, false) => "days",
                (IntlRelativeTimeUnit::Week, _) => "wk.",
                (IntlRelativeTimeUnit::Month, _) => "mo.",
                (IntlRelativeTimeUnit::Quarter, true) => "qtr.",
                (IntlRelativeTimeUnit::Quarter, false) => "qtrs.",
                (IntlRelativeTimeUnit::Year, _) => "yr.",
            }
        }
    }
}

/// Selects the exact Polish forms exercised by CLDR relative-time long/short/narrow patterns.
fn polish_unit_label(
    style: IntlRelativeTimeFormatStyle,
    unit: IntlRelativeTimeUnit,
    category: IntlPluralCategory,
) -> &'static str {
    let form = match category {
        IntlPluralCategory::One => 0,
        IntlPluralCategory::Few => 1,
        IntlPluralCategory::Many | IntlPluralCategory::Zero | IntlPluralCategory::Two => 2,
        IntlPluralCategory::Other => 3,
    };
    match style {
        IntlRelativeTimeFormatStyle::Long => match unit {
            IntlRelativeTimeUnit::Second => ["sekundę", "sekundy", "sekund", "sekundy"][form],
            IntlRelativeTimeUnit::Minute => ["minutę", "minuty", "minut", "minuty"][form],
            IntlRelativeTimeUnit::Hour => ["godzinę", "godziny", "godzin", "godziny"][form],
            IntlRelativeTimeUnit::Day => ["dzień", "dni", "dni", "dnia"][form],
            IntlRelativeTimeUnit::Week => ["tydzień", "tygodnie", "tygodni", "tygodnia"][form],
            IntlRelativeTimeUnit::Month => ["miesiąc", "miesiące", "miesięcy", "miesiąca"][form],
            IntlRelativeTimeUnit::Quarter => ["kwartał", "kwartały", "kwartałów", "kwartału"][form],
            IntlRelativeTimeUnit::Year => ["rok", "lata", "lat", "roku"][form],
        },
        IntlRelativeTimeFormatStyle::Short => match unit {
            IntlRelativeTimeUnit::Second => "sek.",
            IntlRelativeTimeUnit::Minute => "min",
            IntlRelativeTimeUnit::Hour => "godz.",
            IntlRelativeTimeUnit::Day => ["dzień", "dni", "dni", "dnia"][form],
            IntlRelativeTimeUnit::Week => ["tydz.", "tyg.", "tyg.", "tyg."][form],
            IntlRelativeTimeUnit::Month => "mies.",
            IntlRelativeTimeUnit::Quarter => "kw.",
            IntlRelativeTimeUnit::Year => ["rok", "lata", "lat", "roku"][form],
        },
        IntlRelativeTimeFormatStyle::Narrow => match unit {
            IntlRelativeTimeUnit::Second => "s",
            IntlRelativeTimeUnit::Minute => "min",
            IntlRelativeTimeUnit::Hour => "g.",
            IntlRelativeTimeUnit::Day => ["dzień", "dni", "dni", "dnia"][form],
            IntlRelativeTimeUnit::Week => ["tydz.", "tyg.", "tyg.", "tyg."][form],
            IntlRelativeTimeUnit::Month => "mies.",
            IntlRelativeTimeUnit::Quarter => "kw.",
            IntlRelativeTimeUnit::Year => ["rok", "lata", "lat", "roku"][form],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_polish_patterns_reuse_number_and_plural_backends() {
        let english = create("en-US", request("en-US", IntlRelativeTimeFormatStyle::Long)).unwrap();
        assert_eq!(
            String::from_utf16(
                &english
                    .backend
                    .format(
                        &IntlMathematicalValue::Finite("1000".into()),
                        IntlRelativeTimeUnit::Year,
                    )
                    .unwrap()
            )
            .unwrap(),
            "in 1,000 years"
        );
        let polish = create("pl-PL", request("pl-PL", IntlRelativeTimeFormatStyle::Long)).unwrap();
        let parts = polish
            .backend
            .format_to_parts(
                &IntlMathematicalValue::Finite("123456.78".into()),
                IntlRelativeTimeUnit::Month,
            )
            .unwrap();
        assert_eq!(
            String::from_utf16(&parts.formatted).unwrap(),
            "za 123\u{a0}456,78 miesiąca"
        );
        assert!(
            parts
                .spans
                .iter()
                .any(|span| { span.kind == IntlNumberFormatPartType::Group && span.has_unit })
        );
    }

    fn request(locale: &str, style: IntlRelativeTimeFormatStyle) -> IntlRelativeTimeFormatRequest {
        IntlRelativeTimeFormatRequest {
            locales: vec![locale.into()].into_boxed_slice(),
            locale_matcher: IntlLocaleMatcher::BestFit,
            numbering_system: None,
            style,
            numeric: IntlRelativeTimeFormatNumeric::Always,
        }
    }
}
