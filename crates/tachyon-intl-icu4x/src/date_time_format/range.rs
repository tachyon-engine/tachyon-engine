//! Locale-specific DateTimeFormat interval patterns and typed range assembly.

use tachyon_vm::{
    HostProviderError, IntlDateTimeFormatBackend, IntlDateTimeInput, IntlDateTimeNumericStyle,
    IntlDateTimePartSpan, IntlDateTimePartType, IntlDateTimeRangePartSpan, IntlDateTimeRangeSource,
    IntlFormattedDateTimeParts, IntlFormattedDateTimeRangeParts,
};

use super::{DATA_FAILURE, Icu4xDateTimeFormatBackend, civil_date_time, month_style_is_text};

const RANGE_SEPARATOR: &[u16] = &[0x20, 0x2013, 0x20];

/// Preallocated typed interval output assembled from already-rendered endpoint fields.
struct RenderedDateTimeRange {
    formatted: Vec<u16>,
    spans: Vec<IntlDateTimeRangePartSpan>,
}

/// Applies pinned interval data and preserves complete endpoints for unsupported field layouts.
pub(super) fn format_range_to_parts(
    backend: &Icu4xDateTimeFormatBackend,
    start: IntlDateTimeInput,
    end: IntlDateTimeInput,
) -> Result<IntlFormattedDateTimeRangeParts, HostProviderError> {
    let start_parts = backend.format_to_parts(start)?;
    let end_parts = backend.format_to_parts(end)?;
    if start_parts.formatted == end_parts.formatted {
        return Ok(RenderedDateTimeRange::shared(start_parts)?.finish());
    }
    if !supports_en_us_text_date_range(backend, &start_parts, &end_parts) {
        return Ok(RenderedDateTimeRange::preserved(start_parts, end_parts)?.finish());
    }
    let start_civil = local_civil(start)?;
    let end_civil = local_civil(end)?;
    if start_civil.year != end_civil.year {
        return Ok(RenderedDateTimeRange::preserved(start_parts, end_parts)?.finish());
    }
    Ok(RenderedDateTimeRange::collapsed_en_us_date(
        start_parts,
        end_parts,
        start_civil.month == end_civil.month,
    )?
    .finish())
}

#[inline(always)]
fn local_civil(input: IntlDateTimeInput) -> Result<super::CivilDateTime, HostProviderError> {
    let milliseconds = input
        .utc_milliseconds
        .checked_add(input.offset_milliseconds)
        .ok_or(DATA_FAILURE)?;
    Ok(civil_date_time(milliseconds))
}

/// Restricts the hand-authored interval pattern to its exact CLDR field layout.
fn supports_en_us_text_date_range(
    backend: &Icu4xDateTimeFormatBackend,
    start: &IntlFormattedDateTimeParts,
    end: &IntlFormattedDateTimeParts,
) -> bool {
    backend.locale.starts_with("en-US")
        && backend.calendar.as_ref() == "gregory"
        && backend.options.date_style.is_none()
        && backend.options.time_style.is_none()
        && backend.options.weekday.is_none()
        && backend.options.era.is_none()
        && backend.options.year == Some(IntlDateTimeNumericStyle::Numeric)
        && backend.options.month.is_some_and(month_style_is_text)
        && backend.options.day.is_some()
        && !backend.has_time_fields()
        && has_en_us_text_date_layout(start)
        && has_en_us_text_date_layout(end)
}

#[inline(always)]
fn has_en_us_text_date_layout(parts: &IntlFormattedDateTimeParts) -> bool {
    matches!(
        parts.spans.as_ref(),
        [
            IntlDateTimePartSpan {
                kind: IntlDateTimePartType::Month,
                ..
            },
            IntlDateTimePartSpan {
                kind: IntlDateTimePartType::Literal,
                ..
            },
            IntlDateTimePartSpan {
                kind: IntlDateTimePartType::Day,
                ..
            },
            IntlDateTimePartSpan {
                kind: IntlDateTimePartType::Literal,
                ..
            },
            IntlDateTimePartSpan {
                kind: IntlDateTimePartType::Year,
                ..
            }
        ]
    )
}

impl RenderedDateTimeRange {
    /// Allocates once using the non-collapsed range as the strict output upper bound.
    fn new(
        start: &IntlFormattedDateTimeParts,
        end: &IntlFormattedDateTimeParts,
    ) -> Result<Self, HostProviderError> {
        let unit_capacity = start
            .formatted
            .len()
            .checked_add(RANGE_SEPARATOR.len())
            .and_then(|length| length.checked_add(end.formatted.len()))
            .ok_or(DATA_FAILURE)?;
        let span_capacity = start
            .spans
            .len()
            .checked_add(end.spans.len())
            .and_then(|length| length.checked_add(1))
            .ok_or(DATA_FAILURE)?;
        let mut formatted = Vec::new();
        formatted
            .try_reserve_exact(unit_capacity)
            .map_err(|_| DATA_FAILURE)?;
        let mut spans = Vec::new();
        spans
            .try_reserve_exact(span_capacity)
            .map_err(|_| DATA_FAILURE)?;
        Ok(Self { formatted, spans })
    }

    /// Marks one practically-equal endpoint as entirely shared.
    fn shared(parts: IntlFormattedDateTimeParts) -> Result<Self, HostProviderError> {
        let empty = IntlFormattedDateTimeParts {
            formatted: Box::new([]),
            spans: Box::new([]),
        };
        let mut output = Self::new(&parts, &empty)?;
        for index in 0..parts.spans.len() {
            output.push_part(&parts, index, IntlDateTimeRangeSource::Shared)?;
        }
        Ok(output)
    }

    /// Keeps both complete endpoint patterns when locale data offers no safe collapsing rule.
    fn preserved(
        start: IntlFormattedDateTimeParts,
        end: IntlFormattedDateTimeParts,
    ) -> Result<Self, HostProviderError> {
        let mut output = Self::new(&start, &end)?;
        for index in 0..start.spans.len() {
            output.push_part(&start, index, IntlDateTimeRangeSource::StartRange)?;
        }
        output.push_units(
            IntlDateTimePartType::Literal,
            IntlDateTimeRangeSource::Shared,
            RANGE_SEPARATOR,
        )?;
        for index in 0..end.spans.len() {
            output.push_part(&end, index, IntlDateTimeRangeSource::EndRange)?;
        }
        Ok(output)
    }

    /// Collapses a shared en-US year and, when possible, its textual month prefix.
    fn collapsed_en_us_date(
        start: IntlFormattedDateTimeParts,
        end: IntlFormattedDateTimeParts,
        same_month: bool,
    ) -> Result<Self, HostProviderError> {
        let mut output = Self::new(&start, &end)?;
        if same_month {
            output.push_part(&start, 0, IntlDateTimeRangeSource::Shared)?;
            output.push_part(&start, 1, IntlDateTimeRangeSource::Shared)?;
            output.push_part(&start, 2, IntlDateTimeRangeSource::StartRange)?;
        } else {
            for index in 0..=2 {
                output.push_part(&start, index, IntlDateTimeRangeSource::StartRange)?;
            }
        }
        output.push_units(
            IntlDateTimePartType::Literal,
            IntlDateTimeRangeSource::Shared,
            RANGE_SEPARATOR,
        )?;
        if same_month {
            output.push_part(&end, 2, IntlDateTimeRangeSource::EndRange)?;
        } else {
            for index in 0..=2 {
                output.push_part(&end, index, IntlDateTimeRangeSource::EndRange)?;
            }
        }
        output.push_part(&start, 3, IntlDateTimeRangeSource::Shared)?;
        output.push_part(&start, 4, IntlDateTimeRangeSource::Shared)?;
        Ok(output)
    }

    /// Copies one typed field and rewrites its span to the new interval buffer.
    fn push_part(
        &mut self,
        parts: &IntlFormattedDateTimeParts,
        index: usize,
        source: IntlDateTimeRangeSource,
    ) -> Result<(), HostProviderError> {
        let span = parts.spans.get(index).ok_or(DATA_FAILURE)?;
        let source_start = usize::try_from(span.start).map_err(|_| DATA_FAILURE)?;
        let source_end = usize::try_from(span.end).map_err(|_| DATA_FAILURE)?;
        let units = parts
            .formatted
            .get(source_start..source_end)
            .ok_or(DATA_FAILURE)?;
        self.push_units(span.kind, source, units)
    }

    /// Appends one already-classified field without permitting hidden Vec growth.
    fn push_units(
        &mut self,
        kind: IntlDateTimePartType,
        source: IntlDateTimeRangeSource,
        units: &[u16],
    ) -> Result<(), HostProviderError> {
        let start = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        self.formatted.extend_from_slice(units);
        let end = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        self.spans.push(IntlDateTimeRangePartSpan {
            kind,
            source,
            start,
            end,
        });
        Ok(())
    }

    #[inline(always)]
    fn finish(self) -> IntlFormattedDateTimeRangeParts {
        IntlFormattedDateTimeRangeParts {
            formatted: self.formatted.into_boxed_slice(),
            spans: self.spans.into_boxed_slice(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date_time_format::create;
    use tachyon_vm::{
        IntlDateTimeFormatOptions, IntlDateTimeFormatRequest, IntlDateTimeMonthStyle,
        IntlLocaleMatcher,
    };

    fn request(options: IntlDateTimeFormatOptions) -> IntlDateTimeFormatRequest {
        IntlDateTimeFormatRequest {
            locales: vec!["en-US".into()].into_boxed_slice(),
            locale_matcher: IntlLocaleMatcher::BestFit,
            calendar: None,
            numbering_system: None,
            hour_cycle: None,
            hour12: None,
            time_zone: Some("UTC".into()),
            options,
        }
    }

    #[test]
    /// Verifies en-US interval collapsing and every shared/start/end field boundary.
    fn collapses_en_us_text_date_ranges() {
        let creation = create(
            "en-US",
            request(IntlDateTimeFormatOptions {
                year: Some(IntlDateTimeNumericStyle::Numeric),
                month: Some(IntlDateTimeMonthStyle::Short),
                day: Some(IntlDateTimeNumericStyle::Numeric),
                ..Default::default()
            }),
        )
        .unwrap();
        let input = |utc_milliseconds| IntlDateTimeInput {
            utc_milliseconds,
            offset_milliseconds: 0,
        };
        let january_3 = input(1_546_473_600_000);
        let january_5 = input(1_546_646_400_000);
        let march_4 = input(1_551_657_600_000);
        let march_4_2020 = input(1_583_280_000_000);
        for (start, end, expected) in [
            (january_3, january_5, "Jan 3 – 5, 2019"),
            (january_3, march_4, "Jan 3 – Mar 4, 2019"),
            (january_3, march_4_2020, "Jan 3, 2019 – Mar 4, 2020"),
        ] {
            let formatted = creation.backend.format_range(start, end).unwrap();
            assert_eq!(String::from_utf16(&formatted).unwrap(), expected);
        }
        let parts = creation
            .backend
            .format_range_to_parts(january_3, january_5)
            .unwrap();
        let actual = parts
            .spans
            .iter()
            .map(|span| {
                (
                    span.kind,
                    span.source,
                    String::from_utf16(&parts.formatted[span.start as usize..span.end as usize])
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                (
                    IntlDateTimePartType::Month,
                    IntlDateTimeRangeSource::Shared,
                    "Jan".into(),
                ),
                (
                    IntlDateTimePartType::Literal,
                    IntlDateTimeRangeSource::Shared,
                    " ".into(),
                ),
                (
                    IntlDateTimePartType::Day,
                    IntlDateTimeRangeSource::StartRange,
                    "3".into(),
                ),
                (
                    IntlDateTimePartType::Literal,
                    IntlDateTimeRangeSource::Shared,
                    " – ".into(),
                ),
                (
                    IntlDateTimePartType::Day,
                    IntlDateTimeRangeSource::EndRange,
                    "5".into(),
                ),
                (
                    IntlDateTimePartType::Literal,
                    IntlDateTimeRangeSource::Shared,
                    ", ".into(),
                ),
                (
                    IntlDateTimePartType::Year,
                    IntlDateTimeRangeSource::Shared,
                    "2019".into(),
                ),
            ]
        );
    }
}
