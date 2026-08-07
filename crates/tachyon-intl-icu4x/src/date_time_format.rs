//! ICU4X-locale-backed implementation of Tachyon's provider-neutral DateTimeFormat ABI.

use icu_locale::{
    Locale,
    extensions::unicode::{Key, Value},
};
use tachyon_vm::{
    HostProviderError, IntlDateTimeFormatBackend, IntlDateTimeFormatCreation,
    IntlDateTimeFormatOptions, IntlDateTimeFormatRequest, IntlDateTimeFormatResolved,
    IntlDateTimeHourCycle, IntlDateTimeInput, IntlDateTimeMonthStyle, IntlDateTimeNumericStyle,
    IntlDateTimePartSpan, IntlDateTimePartType, IntlDateTimeStyle, IntlDateTimeTextStyle,
    IntlDateTimeZoneNameStyle, IntlFormattedDateTimeParts, IntlLocaleMatcher,
};

use crate::{
    number_format::load_date_time_decimal_data,
    supported_values::{CALENDARS, NUMBERING_SYSTEMS},
    tuning::{DATE_TIME_INITIAL_CODE_UNITS, DATE_TIME_INITIAL_PARTS},
};

const DATA_FAILURE: HostProviderError = HostProviderError::Failure(3);
const MILLIS_PER_DAY: i64 = 86_400_000;
const MILLIS_PER_HOUR: i64 = 3_600_000;
const MILLIS_PER_MINUTE: i64 = 60_000;
const MILLIS_PER_SECOND: i64 = 1_000;

/// Send-safe resolved pattern state; no ICU payload or shared ownership crosses construction.
struct Icu4xDateTimeFormatBackend {
    locale: Box<str>,
    calendar: Box<str>,
    numbering_system: Box<str>,
    digits: [char; 10],
    decimal_separator: Box<str>,
    time_zone: Box<str>,
    hour_cycle: IntlDateTimeHourCycle,
    options: IntlDateTimeFormatOptions,
}

struct MatchedLocale {
    requested: Locale,
    data_locale: Locale,
}

/// Mutable UTF-16 output shared by the string and parts entry points.
struct RenderedDateTime {
    formatted: Vec<u16>,
    spans: Option<Vec<IntlDateTimePartSpan>>,
}

#[derive(Clone, Copy)]
struct CivilDateTime {
    year: i64,
    month: u8,
    day: u8,
    weekday: u8,
    hour: u8,
    minute: u8,
    second: u8,
    millisecond: u16,
}

impl IntlDateTimeFormatBackend for Icu4xDateTimeFormatBackend {
    fn format(&self, input: IntlDateTimeInput) -> Result<Box<[u16]>, HostProviderError> {
        Ok(self.render(input, false)?.formatted.into_boxed_slice())
    }

    fn format_to_parts(
        &self,
        input: IntlDateTimeInput,
    ) -> Result<IntlFormattedDateTimeParts, HostProviderError> {
        let rendered = self.render(input, true)?;
        Ok(IntlFormattedDateTimeParts {
            formatted: rendered.formatted.into_boxed_slice(),
            spans: rendered.spans.unwrap_or_default().into_boxed_slice(),
        })
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.locale.len()
            + self.calendar.len()
            + self.numbering_system.len()
            + self.decimal_separator.len()
            + self.time_zone.len()
    }
}

impl Icu4xDateTimeFormatBackend {
    /// Converts the host-resolved civil instant once and emits a provider-owned field stream.
    fn render(
        &self,
        input: IntlDateTimeInput,
        collect_parts: bool,
    ) -> Result<RenderedDateTime, HostProviderError> {
        let local_milliseconds = input
            .utc_milliseconds
            .checked_add(input.offset_milliseconds)
            .ok_or(DATA_FAILURE)?;
        let civil = civil_date_time(local_milliseconds);
        let mut output = RenderedDateTime::new(DATE_TIME_INITIAL_CODE_UNITS, collect_parts)?;
        if self.options.date_style.is_some() || self.has_date_fields() {
            self.render_date(&mut output, civil)?;
        }
        if self.options.time_style.is_some() || self.has_time_fields() {
            if !output.formatted.is_empty() {
                output.push(IntlDateTimePartType::Literal, ", ")?;
            }
            self.render_time(&mut output, civil, input.offset_milliseconds)?;
        }
        if output.formatted.is_empty() {
            self.render_date(&mut output, civil)?;
        }
        Ok(output)
    }

    #[inline(always)]
    fn has_date_fields(&self) -> bool {
        self.options.weekday.is_some()
            || self.options.era.is_some()
            || self.options.year.is_some()
            || self.options.month.is_some()
            || self.options.day.is_some()
    }

    #[inline(always)]
    fn has_time_fields(&self) -> bool {
        self.options.day_period.is_some()
            || self.options.hour.is_some()
            || self.options.minute.is_some()
            || self.options.second.is_some()
            || self.options.fractional_second_digits.is_some()
            || self.options.time_zone_name.is_some()
    }

    /// Emits the initial Gregorian date patterns needed by the VM vertical slice.
    fn render_date(
        &self,
        output: &mut RenderedDateTime,
        civil: CivilDateTime,
    ) -> Result<(), HostProviderError> {
        let style = self.options.date_style;
        if self.options.weekday.is_some() || matches!(style, Some(IntlDateTimeStyle::Full)) {
            output.push(
                IntlDateTimePartType::Weekday,
                weekday_name(civil.weekday, self.options.weekday, style),
            )?;
            output.push(IntlDateTimePartType::Literal, ", ")?;
        }
        let month_style = self.options.month.unwrap_or(match style {
            Some(IntlDateTimeStyle::Full | IntlDateTimeStyle::Long) => IntlDateTimeMonthStyle::Long,
            Some(IntlDateTimeStyle::Medium) => IntlDateTimeMonthStyle::Short,
            _ => IntlDateTimeMonthStyle::Numeric,
        });
        let day_style = self
            .options
            .day
            .unwrap_or(IntlDateTimeNumericStyle::Numeric);
        let year_style = self
            .options
            .year
            .unwrap_or(if style == Some(IntlDateTimeStyle::Short) {
                IntlDateTimeNumericStyle::TwoDigit
            } else {
                IntlDateTimeNumericStyle::Numeric
            });
        let year = if year_style == IntlDateTimeNumericStyle::TwoDigit {
            civil.year.rem_euclid(100)
        } else {
            civil.year
        };
        if self.locale.starts_with("en-US") {
            self.push_month(output, civil.month, month_style)?;
            output.push(
                IntlDateTimePartType::Literal,
                if month_style_is_text(month_style) {
                    " "
                } else {
                    "/"
                },
            )?;
            output.push_number(
                IntlDateTimePartType::Day,
                i64::from(civil.day),
                day_style,
                &self.digits,
            )?;
            output.push(
                IntlDateTimePartType::Literal,
                if month_style_is_text(month_style) {
                    ", "
                } else {
                    "/"
                },
            )?;
            output.push_number(IntlDateTimePartType::Year, year, year_style, &self.digits)?;
        } else {
            output.push_number(
                IntlDateTimePartType::Day,
                i64::from(civil.day),
                day_style,
                &self.digits,
            )?;
            output.push(IntlDateTimePartType::Literal, "/")?;
            self.push_month(output, civil.month, month_style)?;
            output.push(IntlDateTimePartType::Literal, "/")?;
            output.push_number(IntlDateTimePartType::Year, year, year_style, &self.digits)?;
        }
        if self.options.era.is_some() || civil.year <= 0 {
            output.push(IntlDateTimePartType::Literal, " ")?;
            output.push(
                IntlDateTimePartType::Era,
                if civil.year <= 0 { "BC" } else { "AD" },
            )?;
        }
        Ok(())
    }

    /// Emits hour-cycle-aware time fields without consulting process locale or time zone state.
    fn render_time(
        &self,
        output: &mut RenderedDateTime,
        civil: CivilDateTime,
        offset_milliseconds: i64,
    ) -> Result<(), HostProviderError> {
        let style = self.options.time_style;
        let include_hour = self.options.hour.is_some() || style.is_some();
        let include_minute = self.options.minute.is_some() || style.is_some();
        let include_second = self.options.second.is_some()
            || matches!(
                style,
                Some(IntlDateTimeStyle::Full | IntlDateTimeStyle::Long | IntlDateTimeStyle::Medium)
            );
        let hour12 = matches!(
            self.hour_cycle,
            IntlDateTimeHourCycle::H11 | IntlDateTimeHourCycle::H12
        );
        if include_hour {
            let hour = displayed_hour(civil.hour, self.hour_cycle);
            output.push_number(
                IntlDateTimePartType::Hour,
                i64::from(hour),
                self.options
                    .hour
                    .unwrap_or(IntlDateTimeNumericStyle::Numeric),
                &self.digits,
            )?;
        }
        if include_minute {
            if include_hour {
                output.push(IntlDateTimePartType::Literal, ":")?;
            }
            output.push_number(
                IntlDateTimePartType::Minute,
                i64::from(civil.minute),
                IntlDateTimeNumericStyle::TwoDigit,
                &self.digits,
            )?;
        }
        if include_second {
            output.push(IntlDateTimePartType::Literal, ":")?;
            output.push_number(
                IntlDateTimePartType::Second,
                i64::from(civil.second),
                IntlDateTimeNumericStyle::TwoDigit,
                &self.digits,
            )?;
        }
        if let Some(digits) = self.options.fractional_second_digits {
            let divisor = match digits {
                1 => 100,
                2 => 10,
                _ => 1,
            };
            output.push(IntlDateTimePartType::Literal, &self.decimal_separator)?;
            output.push_localized_number(
                IntlDateTimePartType::FractionalSecond,
                i64::from(civil.millisecond) / divisor,
                usize::from(digits),
                &self.digits,
            )?;
        }
        if let Some(style) = self.options.day_period {
            if include_hour {
                output.push(IntlDateTimePartType::Literal, " ")?;
            }
            output.push(
                IntlDateTimePartType::DayPeriod,
                flexible_day_period(&self.locale, civil, style),
            )?;
        } else if hour12 && include_hour {
            output.push(IntlDateTimePartType::Literal, " ")?;
            output.push(
                IntlDateTimePartType::DayPeriod,
                if civil.hour < 12 { "AM" } else { "PM" },
            )?;
        }
        let time_zone_name = self.options.time_zone_name.or(match style {
            Some(IntlDateTimeStyle::Full) => Some(IntlDateTimeZoneNameStyle::Long),
            Some(IntlDateTimeStyle::Long) => Some(IntlDateTimeZoneNameStyle::Short),
            _ => None,
        });
        if let Some(style) = time_zone_name {
            output.push(IntlDateTimePartType::Literal, " ")?;
            let label = time_zone_label(&self.time_zone, style, offset_milliseconds);
            output.push(IntlDateTimePartType::TimeZoneName, &label)?;
        }
        Ok(())
    }

    fn push_month(
        &self,
        output: &mut RenderedDateTime,
        month: u8,
        style: IntlDateTimeMonthStyle,
    ) -> Result<(), HostProviderError> {
        match style {
            IntlDateTimeMonthStyle::Numeric => output.push_number(
                IntlDateTimePartType::Month,
                i64::from(month),
                IntlDateTimeNumericStyle::Numeric,
                &self.digits,
            ),
            IntlDateTimeMonthStyle::TwoDigit => output.push_number(
                IntlDateTimePartType::Month,
                i64::from(month),
                IntlDateTimeNumericStyle::TwoDigit,
                &self.digits,
            ),
            IntlDateTimeMonthStyle::Long => output.push(
                IntlDateTimePartType::Month,
                month_name(month, IntlDateTimeTextStyle::Long),
            ),
            IntlDateTimeMonthStyle::Short => output.push(
                IntlDateTimePartType::Month,
                month_name(month, IntlDateTimeTextStyle::Short),
            ),
            IntlDateTimeMonthStyle::Narrow => output.push(
                IntlDateTimePartType::Month,
                month_name(month, IntlDateTimeTextStyle::Narrow),
            ),
        }
    }
}

impl RenderedDateTime {
    fn new(capacity: usize, collect_parts: bool) -> Result<Self, HostProviderError> {
        let mut formatted = Vec::new();
        formatted
            .try_reserve_exact(capacity)
            .map_err(|_| DATA_FAILURE)?;
        let spans = if collect_parts {
            let mut spans = Vec::new();
            spans
                .try_reserve_exact(DATE_TIME_INITIAL_PARTS)
                .map_err(|_| DATA_FAILURE)?;
            Some(spans)
        } else {
            None
        };
        Ok(Self { formatted, spans })
    }

    /// Appends a field and records the exact gap-free UTF-16 span when requested.
    fn push(&mut self, kind: IntlDateTimePartType, text: &str) -> Result<(), HostProviderError> {
        let start = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        self.formatted.extend(text.encode_utf16());
        let end = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        if let Some(spans) = &mut self.spans {
            spans.push(IntlDateTimePartSpan { kind, start, end });
        }
        Ok(())
    }

    fn push_number(
        &mut self,
        kind: IntlDateTimePartType,
        value: i64,
        style: IntlDateTimeNumericStyle,
        digits: &[char; 10],
    ) -> Result<(), HostProviderError> {
        let width = usize::from(style == IntlDateTimeNumericStyle::TwoDigit) + 1;
        self.push_localized_number(kind, value, width, digits)
    }

    /// Formats an integer to the requested width and substitutes the resolved decimal digits.
    fn push_localized_number(
        &mut self,
        kind: IntlDateTimePartType,
        value: i64,
        width: usize,
        digits: &[char; 10],
    ) -> Result<(), HostProviderError> {
        let text = if width == 1 {
            value.to_string()
        } else {
            format!("{value:0width$}")
        };
        let start = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        self.formatted
            .try_reserve(text.len().saturating_mul(2))
            .map_err(|_| DATA_FAILURE)?;
        for byte in text.bytes() {
            if byte.is_ascii_digit() {
                let digit = digits[usize::from(byte - b'0')];
                let mut encoded = [0_u16; 2];
                self.formatted
                    .extend_from_slice(digit.encode_utf16(&mut encoded));
            } else {
                self.formatted.push(u16::from(byte));
            }
        }
        let end = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        if let Some(spans) = &mut self.spans {
            spans.push(IntlDateTimePartSpan { kind, start, end });
        }
        Ok(())
    }
}

/// Resolves locale extensions and scalar defaults without retaining ICU payloads.
pub(super) fn create(
    default_locale: &str,
    mut request: IntlDateTimeFormatRequest,
) -> Result<IntlDateTimeFormatCreation, HostProviderError> {
    let matched = request
        .locales
        .iter()
        .find_map(|locale| match_locale(locale))
        .or_else(|| match_locale(default_locale))
        .ok_or(DATA_FAILURE)?;
    let time_zone = request.time_zone.take().ok_or(DATA_FAILURE)?;
    let extension_calendar = unicode_keyword(&matched.requested, "ca");
    let option_calendar = request
        .calendar
        .take()
        .map(|calendar| canonicalize_calendar_identifier(&calendar));
    let calendar = resolve_supported_keyword(
        option_calendar.as_deref(),
        extension_calendar.as_deref(),
        CALENDARS,
        "gregory",
    );
    let extension_numbering_system = unicode_keyword(&matched.requested, "nu");
    let option_numbering_system = request
        .numbering_system
        .take()
        .map(|numbering_system| numbering_system.to_ascii_lowercase().into_boxed_str());
    let numbering_system = resolve_supported_keyword(
        option_numbering_system.as_deref(),
        extension_numbering_system.as_deref(),
        NUMBERING_SYSTEMS,
        "latn",
    );
    let extension_hour_cycle = unicode_keyword(&matched.requested, "hc")
        .as_deref()
        .and_then(parse_hour_cycle);
    let locale_hour_cycle = default_hour_cycle(&matched.data_locale);
    let hour_cycle = match request.hour12 {
        Some(true) => hour_cycle12(&matched.data_locale),
        Some(false) => IntlDateTimeHourCycle::H23,
        None => request
            .hour_cycle
            .or(extension_hour_cycle)
            .unwrap_or(locale_hour_cycle),
    };
    let mut resolved_locale = matched.data_locale.clone();
    if extension_calendar.as_deref() == Some(calendar.as_ref()) {
        set_unicode_keyword(&mut resolved_locale, "ca", &calendar)?;
    }
    if extension_numbering_system.as_deref() == Some(numbering_system.as_ref()) {
        set_unicode_keyword(&mut resolved_locale, "nu", &numbering_system)?;
    }
    if request.hour12.is_none()
        && extension_hour_cycle == Some(hour_cycle)
        && request.hour_cycle.is_none_or(|option| option == hour_cycle)
    {
        set_unicode_keyword(&mut resolved_locale, "hc", hour_cycle_name(hour_cycle))?;
    }
    if !has_any_component(&request.options)
        && request.options.date_style.is_none()
        && request.options.time_style.is_none()
    {
        request.options.year = Some(IntlDateTimeNumericStyle::Numeric);
        request.options.month = Some(IntlDateTimeMonthStyle::Numeric);
        request.options.day = Some(IntlDateTimeNumericStyle::Numeric);
    }
    let decimal_data = load_date_time_decimal_data(&matched.data_locale, &numbering_system)?;
    let resolved = IntlDateTimeFormatResolved {
        locale: resolved_locale.to_string().into_boxed_str(),
        calendar: calendar.clone(),
        numbering_system: numbering_system.clone(),
        time_zone: time_zone.clone(),
        hour_cycle: (request.options.hour.is_some() || request.options.time_style.is_some())
            .then_some(hour_cycle),
        options: request.options.clone(),
    };
    Ok(IntlDateTimeFormatCreation {
        backend: Box::new(Icu4xDateTimeFormatBackend {
            locale: matched.data_locale.to_string().into_boxed_str(),
            calendar,
            numbering_system,
            digits: decimal_data.digits,
            decimal_separator: decimal_data.decimal_separator,
            time_zone,
            hour_cycle,
            options: request.options,
        }),
        resolved,
    })
}

/// Filters canonical requested spellings while rejecting language-neutral tags.
pub(super) fn supported_locales(
    locales: &[Box<str>],
    _matcher: IntlLocaleMatcher,
) -> Box<[Box<str>]> {
    locales
        .iter()
        .filter(|locale| match_locale(locale).is_some())
        .cloned()
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn match_locale(locale: &str) -> Option<MatchedLocale> {
    let requested = locale.parse::<Locale>().ok()?;
    let mut data_locale = requested.clone();
    data_locale.extensions = Default::default();
    if matches!(data_locale.id.language.as_str(), "und" | "zxx") {
        return None;
    }
    Some(MatchedLocale {
        requested,
        data_locale,
    })
}

fn canonicalize_calendar_identifier(calendar: &str) -> Box<str> {
    let calendar = calendar.to_ascii_lowercase();
    if calendar == "islamicc" {
        "islamic-civil".into()
    } else {
        calendar.into_boxed_str()
    }
}

fn resolve_supported_keyword(
    option: Option<&str>,
    extension: Option<&str>,
    supported: &[&str],
    default: &str,
) -> Box<str> {
    option
        .filter(|value| supported.binary_search(value).is_ok())
        .or_else(|| extension.filter(|value| supported.binary_search(value).is_ok()))
        .unwrap_or(default)
        .into()
}

fn unicode_keyword(locale: &Locale, key: &str) -> Option<Box<str>> {
    let key = key.parse::<Key>().ok()?;
    locale
        .extensions
        .unicode
        .keywords
        .get(&key)
        .map(|value| value.to_string().into_boxed_str())
}

fn set_unicode_keyword(
    locale: &mut Locale,
    key: &str,
    value: &str,
) -> Result<(), HostProviderError> {
    let key = key.parse::<Key>().map_err(|_| DATA_FAILURE)?;
    let value = value.parse::<Value>().map_err(|_| DATA_FAILURE)?;
    locale.extensions.unicode.keywords.set(key, value);
    Ok(())
}

#[inline(always)]
fn parse_hour_cycle(value: &str) -> Option<IntlDateTimeHourCycle> {
    match value {
        "h11" => Some(IntlDateTimeHourCycle::H11),
        "h12" => Some(IntlDateTimeHourCycle::H12),
        "h23" => Some(IntlDateTimeHourCycle::H23),
        "h24" => Some(IntlDateTimeHourCycle::H24),
        _ => None,
    }
}

#[inline(always)]
const fn hour_cycle_name(value: IntlDateTimeHourCycle) -> &'static str {
    match value {
        IntlDateTimeHourCycle::H11 => "h11",
        IntlDateTimeHourCycle::H12 => "h12",
        IntlDateTimeHourCycle::H23 => "h23",
        IntlDateTimeHourCycle::H24 => "h24",
    }
}

fn default_hour_cycle(locale: &Locale) -> IntlDateTimeHourCycle {
    match locale.id.language.as_str() {
        "en" | "ar" | "hi" => IntlDateTimeHourCycle::H12,
        _ => IntlDateTimeHourCycle::H23,
    }
}

fn hour_cycle12(locale: &Locale) -> IntlDateTimeHourCycle {
    if locale.id.language.as_str() == "ja" {
        IntlDateTimeHourCycle::H11
    } else {
        IntlDateTimeHourCycle::H12
    }
}

/// Converts Unix milliseconds to proleptic-Gregorian civil fields using Euclidean arithmetic.
fn civil_date_time(milliseconds: i64) -> CivilDateTime {
    let days = milliseconds.div_euclid(MILLIS_PER_DAY);
    let day_millis = milliseconds.rem_euclid(MILLIS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    CivilDateTime {
        year,
        month,
        day,
        weekday: u8::try_from((days + 4).rem_euclid(7)).expect("weekday is within 0..7"),
        hour: u8::try_from(day_millis / MILLIS_PER_HOUR).expect("hour is within 0..24"),
        minute: u8::try_from((day_millis % MILLIS_PER_HOUR) / MILLIS_PER_MINUTE)
            .expect("minute is within 0..60"),
        second: u8::try_from((day_millis % MILLIS_PER_MINUTE) / MILLIS_PER_SECOND)
            .expect("second is within 0..60"),
        millisecond: u16::try_from(day_millis % MILLIS_PER_SECOND)
            .expect("millisecond is within 0..1000"),
    }
}

/// Howard Hinnant's era decomposition, shifted so day zero is 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (
        year,
        u8::try_from(month).expect("civil month is within 1..13"),
        u8::try_from(day).expect("civil day is within 1..32"),
    )
}

fn displayed_hour(hour: u8, cycle: IntlDateTimeHourCycle) -> u8 {
    match cycle {
        IntlDateTimeHourCycle::H11 => hour % 12,
        IntlDateTimeHourCycle::H12 => match hour % 12 {
            0 => 12,
            hour => hour,
        },
        IntlDateTimeHourCycle::H23 => hour,
        IntlDateTimeHourCycle::H24 => match hour {
            0 => 24,
            hour => hour,
        },
    }
}

#[inline(always)]
fn month_style_is_text(style: IntlDateTimeMonthStyle) -> bool {
    matches!(
        style,
        IntlDateTimeMonthStyle::Long
            | IntlDateTimeMonthStyle::Short
            | IntlDateTimeMonthStyle::Narrow
    )
}

/// Selects the current English overlay month name without allocating on the format path.
fn month_name(month: u8, style: IntlDateTimeTextStyle) -> &'static str {
    const LONG: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const SHORT: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const NARROW: [&str; 12] = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"];
    let index = usize::from(month.saturating_sub(1)).min(11);
    match style {
        IntlDateTimeTextStyle::Long => LONG[index],
        IntlDateTimeTextStyle::Short => SHORT[index],
        IntlDateTimeTextStyle::Narrow => NARROW[index],
    }
}

/// Selects the current English overlay weekday width used by the initial backend.
fn weekday_name(
    weekday: u8,
    style: Option<IntlDateTimeTextStyle>,
    date_style: Option<IntlDateTimeStyle>,
) -> &'static str {
    const LONG: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const NARROW: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
    let index = usize::from(weekday).min(6);
    match style {
        Some(IntlDateTimeTextStyle::Narrow) => NARROW[index],
        Some(IntlDateTimeTextStyle::Short) => SHORT[index],
        Some(IntlDateTimeTextStyle::Long) | None if date_style == Some(IntlDateTimeStyle::Full) => {
            LONG[index]
        }
        _ => SHORT[index],
    }
}

/// Returns the CLDR English flexible day-period label used by the pinned release data.
fn flexible_day_period(
    locale: &str,
    civil: CivilDateTime,
    style: IntlDateTimeTextStyle,
) -> &'static str {
    if locale != "en" && !locale.starts_with("en-") {
        return if civil.hour < 12 { "AM" } else { "PM" };
    }
    let exact_noon =
        civil.hour == 12 && civil.minute == 0 && civil.second == 0 && civil.millisecond == 0;
    match civil.hour {
        6..=11 => "in the morning",
        12 if exact_noon && style == IntlDateTimeTextStyle::Narrow => "n",
        12 if exact_noon => "noon",
        12..=17 => "in the afternoon",
        18..=20 => "in the evening",
        _ => "at night",
    }
}

#[inline(always)]
fn has_any_component(options: &IntlDateTimeFormatOptions) -> bool {
    options.weekday.is_some()
        || options.era.is_some()
        || options.year.is_some()
        || options.month.is_some()
        || options.day.is_some()
        || options.day_period.is_some()
        || options.hour.is_some()
        || options.minute.is_some()
        || options.second.is_some()
        || options.fractional_second_digits.is_some()
        || options.time_zone_name.is_some()
}

/// Produces deterministic GMT-offset forms while retaining named-zone labels for generic styles.
fn time_zone_label(
    time_zone: &str,
    style: IntlDateTimeZoneNameStyle,
    offset_milliseconds: i64,
) -> String {
    if !matches!(
        style,
        IntlDateTimeZoneNameStyle::ShortOffset | IntlDateTimeZoneNameStyle::LongOffset
    ) {
        return if time_zone.eq_ignore_ascii_case("UTC") {
            if style == IntlDateTimeZoneNameStyle::Long {
                "Coordinated Universal Time".to_owned()
            } else {
                "UTC".to_owned()
            }
        } else {
            time_zone.to_owned()
        };
    }
    if offset_milliseconds == 0 {
        return "GMT".to_owned();
    }
    let sign = if offset_milliseconds < 0 { '-' } else { '+' };
    let absolute_minutes = offset_milliseconds.unsigned_abs() / 60_000;
    let hours = absolute_minutes / 60;
    let minutes = absolute_minutes % 60;
    if style == IntlDateTimeZoneNameStyle::ShortOffset && minutes == 0 {
        format!("GMT{sign}{hours}")
    } else {
        format!("GMT{sign}{hours:02}:{minutes:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(options: IntlDateTimeFormatOptions) -> IntlDateTimeFormatRequest {
        request_for("en-US", options)
    }

    fn request_for(locale: &str, options: IntlDateTimeFormatOptions) -> IntlDateTimeFormatRequest {
        IntlDateTimeFormatRequest {
            locales: vec![locale.into()].into_boxed_slice(),
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
    fn formats_default_date_and_gap_free_parts() {
        let creation = create("en-US", request(IntlDateTimeFormatOptions::default())).unwrap();
        let input = IntlDateTimeInput {
            utc_milliseconds: 0,
            offset_milliseconds: 0,
        };
        assert_eq!(
            String::from_utf16(&creation.backend.format(input).unwrap()).unwrap(),
            "1/1/1970"
        );
        let parts = creation.backend.format_to_parts(input).unwrap();
        assert_eq!(parts.spans.first().unwrap().start, 0);
        assert_eq!(
            usize::try_from(parts.spans.last().unwrap().end).unwrap(),
            parts.formatted.len()
        );
        assert!(
            parts
                .spans
                .windows(2)
                .all(|pair| pair[0].end == pair[1].start)
        );
    }

    #[test]
    fn formats_time_with_hour_cycle_and_fraction() {
        let options = IntlDateTimeFormatOptions {
            hour: Some(IntlDateTimeNumericStyle::Numeric),
            minute: Some(IntlDateTimeNumericStyle::TwoDigit),
            second: Some(IntlDateTimeNumericStyle::TwoDigit),
            fractional_second_digits: Some(3),
            ..IntlDateTimeFormatOptions::default()
        };
        let creation = create("en-US", request(options)).unwrap();
        let formatted = creation
            .backend
            .format(IntlDateTimeInput {
                utc_milliseconds: 45_296_789,
                offset_milliseconds: 0,
            })
            .unwrap();
        assert_eq!(String::from_utf16(&formatted).unwrap(), "12:34:56.789 PM");
    }

    #[test]
    fn filters_language_neutral_locales() {
        let locales = vec!["en-US".into(), "zxx".into(), "fr".into()];
        assert_eq!(
            supported_locales(&locales, IntlLocaleMatcher::Lookup).as_ref(),
            [Box::<str>::from("en-US"), Box::<str>::from("fr")]
        );
    }

    #[test]
    /// Covers hc extension retention, option precedence, and hour12 locale data selection.
    fn resolves_hour_cycle_and_unicode_extension_precedence() {
        let hour = IntlDateTimeFormatOptions {
            hour: Some(IntlDateTimeNumericStyle::Numeric),
            ..IntlDateTimeFormatOptions::default()
        };
        let extension = create("en-US", request_for("de-u-hc-h24", hour.clone())).unwrap();
        assert_eq!(extension.resolved.locale.as_ref(), "de-u-hc-h24");
        assert_eq!(
            extension.resolved.hour_cycle,
            Some(IntlDateTimeHourCycle::H24)
        );

        let mut different_option = request_for("de-u-hc-h24", hour.clone());
        different_option.hour_cycle = Some(IntlDateTimeHourCycle::H23);
        let different_option = create("en-US", different_option).unwrap();
        assert_eq!(different_option.resolved.locale.as_ref(), "de");
        assert_eq!(
            different_option.resolved.hour_cycle,
            Some(IntlDateTimeHourCycle::H23)
        );

        let mut same_option = request_for("de-u-hc-h24", hour.clone());
        same_option.hour_cycle = Some(IntlDateTimeHourCycle::H24);
        assert_eq!(
            create("en-US", same_option)
                .unwrap()
                .resolved
                .locale
                .as_ref(),
            "de-u-hc-h24"
        );

        let mut hour12 = request_for("ja-u-hc-h24", hour.clone());
        hour12.hour12 = Some(true);
        let hour12 = create("en-US", hour12).unwrap();
        assert_eq!(hour12.resolved.locale.as_ref(), "ja");
        assert_eq!(hour12.resolved.hour_cycle, Some(IntlDateTimeHourCycle::H11));

        let mut hour24 = request_for("en-US", hour);
        hour24.hour12 = Some(false);
        assert_eq!(
            create("en-US", hour24).unwrap().resolved.hour_cycle,
            Some(IntlDateTimeHourCycle::H23)
        );
    }

    #[test]
    /// Covers supported/unsupported option precedence and calendar option canonicalization.
    fn resolves_calendar_and_numbering_system_unicode_extensions() {
        let mut extension_calendar =
            request_for("en-u-ca-iso8601", IntlDateTimeFormatOptions::default());
        extension_calendar.calendar = Some("invalid".into());
        let extension_calendar = create("en-US", extension_calendar).unwrap().resolved;
        assert_eq!(extension_calendar.locale.as_ref(), "en-u-ca-iso8601");
        assert_eq!(extension_calendar.calendar.as_ref(), "iso8601");

        let mut option_calendar =
            request_for("en-u-ca-gregory", IntlDateTimeFormatOptions::default());
        option_calendar.calendar = Some("ISO8601".into());
        let option_calendar = create("en-US", option_calendar).unwrap().resolved;
        assert_eq!(option_calendar.locale.as_ref(), "en");
        assert_eq!(option_calendar.calendar.as_ref(), "iso8601");

        let mut alias = request(IntlDateTimeFormatOptions::default());
        alias.calendar = Some("islamicc".into());
        assert_eq!(
            create("en-US", alias).unwrap().resolved.calendar.as_ref(),
            "islamic-civil"
        );

        let mut extension_numbering =
            request_for("en-u-nu-arab", IntlDateTimeFormatOptions::default());
        extension_numbering.numbering_system = Some("invalid".into());
        let extension_numbering = create("en-US", extension_numbering).unwrap().resolved;
        assert_eq!(extension_numbering.locale.as_ref(), "en-u-nu-arab");
        assert_eq!(extension_numbering.numbering_system.as_ref(), "arab");

        let mut option_numbering =
            request_for("en-u-nu-latn", IntlDateTimeFormatOptions::default());
        option_numbering.numbering_system = Some("ARAB".into());
        let option_numbering = create("en-US", option_numbering).unwrap().resolved;
        assert_eq!(option_numbering.locale.as_ref(), "en");
        assert_eq!(option_numbering.numbering_system.as_ref(), "arab");
    }

    #[test]
    fn formats_date_time_fields_with_resolved_numbering_system() {
        let options = IntlDateTimeFormatOptions {
            hour: Some(IntlDateTimeNumericStyle::Numeric),
            minute: Some(IntlDateTimeNumericStyle::Numeric),
            second: Some(IntlDateTimeNumericStyle::Numeric),
            fractional_second_digits: Some(3),
            ..IntlDateTimeFormatOptions::default()
        };
        let creation = create("en-US", request_for("en-US-u-nu-arab", options)).unwrap();
        let formatted = creation
            .backend
            .format(IntlDateTimeInput {
                utc_milliseconds: 9_306_789,
                offset_milliseconds: 0,
            })
            .unwrap();
        assert_eq!(String::from_utf16(&formatted).unwrap(), "٢:٣٥:٠٦٫٧٨٩ AM");
    }

    #[test]
    /// Covers every English transition plus exact-noon and narrow-width behavior.
    fn formats_english_flexible_day_period_boundaries() {
        let options = IntlDateTimeFormatOptions {
            day_period: Some(IntlDateTimeTextStyle::Long),
            ..IntlDateTimeFormatOptions::default()
        };
        let creation = create("en-US", request_for("en", options)).unwrap();
        for (hour, expected) in [
            (0, "at night"),
            (6, "in the morning"),
            (12, "noon"),
            (13, "in the afternoon"),
            (18, "in the evening"),
            (21, "at night"),
        ] {
            let formatted = creation
                .backend
                .format(IntlDateTimeInput {
                    utc_milliseconds: i64::from(hour) * MILLIS_PER_HOUR,
                    offset_milliseconds: 0,
                })
                .unwrap();
            assert_eq!(String::from_utf16(&formatted).unwrap(), expected);
        }
        let afternoon = creation
            .backend
            .format(IntlDateTimeInput {
                utc_milliseconds: 12 * MILLIS_PER_HOUR + 30 * MILLIS_PER_MINUTE,
                offset_milliseconds: 0,
            })
            .unwrap();
        assert_eq!(String::from_utf16(&afternoon).unwrap(), "in the afternoon");

        let options = IntlDateTimeFormatOptions {
            day_period: Some(IntlDateTimeTextStyle::Narrow),
            hour: Some(IntlDateTimeNumericStyle::Numeric),
            ..IntlDateTimeFormatOptions::default()
        };
        let creation = create("en-US", request_for("en", options)).unwrap();
        let formatted = creation
            .backend
            .format(IntlDateTimeInput {
                utc_milliseconds: 12 * MILLIS_PER_HOUR,
                offset_milliseconds: 0,
            })
            .unwrap();
        assert_eq!(String::from_utf16(&formatted).unwrap(), "12 n");
    }

    #[test]
    /// Covers style-implied two-digit years and full UTC zone-name emission.
    fn formats_short_date_and_long_utc_time_style_defaults() {
        let short_date = create(
            "en-US",
            request(IntlDateTimeFormatOptions {
                date_style: Some(IntlDateTimeStyle::Short),
                ..IntlDateTimeFormatOptions::default()
            }),
        )
        .unwrap();
        let input = IntlDateTimeInput {
            utc_milliseconds: 0,
            offset_milliseconds: 0,
        };
        assert_eq!(
            String::from_utf16(&short_date.backend.format(input).unwrap()).unwrap(),
            "1/1/70"
        );

        let full_time = create(
            "en-US",
            request(IntlDateTimeFormatOptions {
                time_style: Some(IntlDateTimeStyle::Full),
                ..IntlDateTimeFormatOptions::default()
            }),
        )
        .unwrap();
        assert_eq!(
            String::from_utf16(&full_time.backend.format(input).unwrap()).unwrap(),
            "12:00:00 AM Coordinated Universal Time"
        );
    }
}
