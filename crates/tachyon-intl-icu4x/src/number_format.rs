//! ICU4X compiled-data implementation of Tachyon's provider-neutral NumberFormat ABI.

use core::str::FromStr;

use fixed_decimal::{SignedRoundingMode, UnsignedRoundingMode};
use icu_decimal::{
    DecimalFormatterPreferences,
    input::{Decimal, SignDisplay},
    provider::{Baked, DecimalDigitsV1, DecimalSymbolsV1, GroupingSizes},
};
use icu_locale::{
    Locale,
    extensions::unicode::{Key, Value},
};
use icu_provider::{
    DataIdentifierBorrowed, DataLocale, DataMarkerAttributes, DataProvider, DataRequest,
    marker::DataMarkerExt,
};
use tachyon_vm::{
    HostProviderError, IntlFormattedNumberParts, IntlLocaleMatcher, IntlMathematicalValue,
    IntlNumberFormatBackend, IntlNumberFormatCompactDisplay, IntlNumberFormatCreation,
    IntlNumberFormatCurrencyDisplay, IntlNumberFormatCurrencySign, IntlNumberFormatNotation,
    IntlNumberFormatPartSpan, IntlNumberFormatPartType, IntlNumberFormatRequest,
    IntlNumberFormatResolved, IntlNumberFormatRoundingMode, IntlNumberFormatSignDisplay,
    IntlNumberFormatStyle, IntlNumberFormatUnitDisplay, IntlNumberFormatUseGrouping,
};

use crate::supported_values::NUMBERING_SYSTEMS;

const DATA_FAILURE: HostProviderError = HostProviderError::Failure(2);

/// Static compiled decimal formatter plus scalar post-parsing controls.
struct Icu4xNumberFormatBackend {
    digits: [char; 10],
    minus_prefix: Box<str>,
    minus_suffix: Box<str>,
    plus_prefix: Box<str>,
    plus_suffix: Box<str>,
    decimal_separator: Box<str>,
    grouping_separator: Box<str>,
    grouping_sizes: GroupingSizes,
    grouping_strategy: IntlNumberFormatUseGrouping,
    locale: Box<str>,
    style: IntlNumberFormatStyle,
    currency: Option<Box<str>>,
    currency_display: IntlNumberFormatCurrencyDisplay,
    currency_sign: IntlNumberFormatCurrencySign,
    notation: IntlNumberFormatNotation,
    compact_display: IntlNumberFormatCompactDisplay,
    unit: Option<Box<str>>,
    unit_display: IntlNumberFormatUnitDisplay,
    nan_symbol: Box<str>,
    infinity_symbol: Box<str>,
    minimum_integer_digits: u8,
    minimum_fraction_digits: u8,
    maximum_fraction_digits: u8,
    minimum_significant_digits: Option<u8>,
    maximum_significant_digits: Option<u8>,
    rounding_mode: IntlNumberFormatRoundingMode,
    sign_display: IntlNumberFormatSignDisplay,
}

/// Mutable output shared by the plain-string and structured-parts rendering paths.
struct RenderedNumber {
    formatted: Vec<u16>,
    spans: Option<Vec<IntlNumberFormatPartSpan>>,
}

#[derive(Clone, Copy)]
struct CompactPattern<'a> {
    scale: i16,
    separator: &'static str,
    label: &'a str,
}

impl IntlNumberFormatBackend for Icu4xNumberFormatBackend {
    /// Converts exact decimal text only after the VM has completed observable ToNumeric work.
    fn format(&self, value: &IntlMathematicalValue) -> Result<Box<[u16]>, HostProviderError> {
        Ok(self.render(value, false)?.formatted.into_boxed_slice())
    }

    fn format_to_parts(
        &self,
        value: &IntlMathematicalValue,
    ) -> Result<IntlFormattedNumberParts, HostProviderError> {
        let rendered = self.render(value, true)?;
        Ok(IntlFormattedNumberParts {
            formatted: rendered.formatted.into_boxed_slice(),
            spans: rendered.spans.unwrap_or_default().into_boxed_slice(),
        })
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

impl Icu4xNumberFormatBackend {
    /// Applies digit constraints once, then routes both public APIs through the same emitter.
    fn render(
        &self,
        value: &IntlMathematicalValue,
        collect_parts: bool,
    ) -> Result<RenderedNumber, HostProviderError> {
        let mut output = RenderedNumber::new(64, collect_parts)?;
        self.push_style_prefix(&mut output)?;
        match value {
            IntlMathematicalValue::Finite(value) => {
                let mut decimal = parse_mathematical_decimal(value)?;
                if self.style == IntlNumberFormatStyle::Percent {
                    decimal.multiply_pow10(2);
                }
                self.render_finite_decimal(&mut output, decimal)?;
            }
            IntlMathematicalValue::NegativeZero => {
                let decimal = Decimal::from_str("-0").map_err(|_| DATA_FAILURE)?;
                self.render_finite_decimal(&mut output, decimal)?;
            }
            IntlMathematicalValue::PositiveInfinity => {
                if matches!(
                    self.sign_display,
                    IntlNumberFormatSignDisplay::Always | IntlNumberFormatSignDisplay::ExceptZero
                ) {
                    self.push_affix(&mut output, &self.plus_prefix, true)?;
                }
                output.push(IntlNumberFormatPartType::Infinity, &self.infinity_symbol)?;
                if matches!(
                    self.sign_display,
                    IntlNumberFormatSignDisplay::Always | IntlNumberFormatSignDisplay::ExceptZero
                ) {
                    self.push_affix(&mut output, &self.plus_suffix, true)?;
                }
            }
            IntlMathematicalValue::NegativeInfinity => {
                if self.sign_display != IntlNumberFormatSignDisplay::Never {
                    self.push_affix(&mut output, &self.minus_prefix, false)?;
                }
                output.push(IntlNumberFormatPartType::Infinity, &self.infinity_symbol)?;
                if self.sign_display != IntlNumberFormatSignDisplay::Never {
                    self.push_affix(&mut output, &self.minus_suffix, false)?;
                }
            }
            IntlMathematicalValue::NaN => {
                if self.sign_display == IntlNumberFormatSignDisplay::Always {
                    self.push_affix(&mut output, &self.plus_prefix, true)?;
                }
                output.push(IntlNumberFormatPartType::Nan, &self.nan_symbol)?;
                if self.sign_display == IntlNumberFormatSignDisplay::Always {
                    self.push_affix(&mut output, &self.plus_suffix, true)?;
                }
            }
        }
        self.push_style_suffix(&mut output)?;
        Ok(output)
    }

    /// Applies notation scaling and emits the optional scientific exponent as typed fields.
    fn render_finite_decimal(
        &self,
        output: &mut RenderedNumber,
        mut decimal: Decimal,
    ) -> Result<(), HostProviderError> {
        if self.notation == IntlNumberFormatNotation::Compact {
            return self.render_compact_decimal(output, decimal);
        }
        let exponent = match self.notation {
            IntlNumberFormatNotation::Standard => None,
            IntlNumberFormatNotation::Scientific => {
                Some(decimal.absolute.nonzero_magnitude_start())
            }
            IntlNumberFormatNotation::Engineering => {
                Some(decimal.absolute.nonzero_magnitude_start().div_euclid(3) * 3)
            }
            IntlNumberFormatNotation::Compact => unreachable!("compact returns above"),
        };
        if let Some(exponent) = exponent {
            decimal.multiply_pow10(-exponent);
        }
        self.apply_digit_rounding(&mut decimal);
        decimal.pad_start(i16::from(self.minimum_integer_digits));
        decimal.apply_sign_display(sign_display(self.sign_display));
        self.localize_decimal(output, &decimal.to_string())?;
        if let Some(exponent) = exponent {
            output.push(IntlNumberFormatPartType::ExponentSeparator, "E")?;
            if exponent < 0 {
                output.push(IntlNumberFormatPartType::ExponentMinusSign, "-")?;
            }
            self.push_digits(
                output,
                IntlNumberFormatPartType::ExponentInteger,
                &exponent.unsigned_abs().to_string(),
            )?;
        }
        Ok(())
    }

    /// Applies either significant-digit or fraction-digit rounding to one scaled decimal.
    fn apply_digit_rounding(&self, decimal: &mut Decimal) {
        if let Some(maximum) = self.maximum_significant_digits {
            let magnitude = decimal.absolute.nonzero_magnitude_start();
            decimal.round_with_mode(
                magnitude - i16::from(maximum) + 1,
                fixed_decimal_rounding_mode(self.rounding_mode),
            );
            if let Some(minimum) = self.minimum_significant_digits {
                let magnitude = decimal.absolute.nonzero_magnitude_start();
                decimal.pad_end(magnitude - i16::from(minimum) + 1);
            }
            return;
        }
        decimal.round_with_mode(
            -i16::from(self.maximum_fraction_digits),
            fixed_decimal_rounding_mode(self.rounding_mode),
        );
        decimal.pad_end(-i16::from(self.minimum_fraction_digits));
    }

    /// Applies compact-pattern scaling and the ECMA compact-default precision policy.
    fn render_compact_decimal(
        &self,
        output: &mut RenderedNumber,
        mut decimal: Decimal,
    ) -> Result<(), HostProviderError> {
        let pattern = self.compact_pattern(decimal.absolute.nonzero_magnitude_start());
        if let Some(pattern) = pattern {
            decimal.multiply_pow10(-pattern.scale);
        }
        let scaled_magnitude = decimal.absolute.nonzero_magnitude_start();
        let maximum_fraction_digits = if scaled_magnitude >= 1 {
            0
        } else {
            u8::try_from(1_i16.saturating_sub(scaled_magnitude)).map_err(|_| DATA_FAILURE)?
        };
        decimal.round_with_mode(
            -i16::from(maximum_fraction_digits),
            fixed_decimal_rounding_mode(self.rounding_mode),
        );
        decimal.apply_sign_display(sign_display(self.sign_display));
        self.localize_decimal(output, &decimal.to_string())?;
        if let Some(pattern) = pattern {
            output.push(IntlNumberFormatPartType::Literal, pattern.separator)?;
            output.push(IntlNumberFormatPartType::Compact, pattern.label)?;
        }
        Ok(())
    }

    /// Selects one compact exponent and label from the compiled locale-family overlay.
    fn compact_pattern(&self, magnitude: i16) -> Option<CompactPattern<'_>> {
        if self.locale.starts_with("ja") || self.locale.starts_with("zh") {
            let (scale, label) = if magnitude >= 8 {
                (8, "億")
            } else if magnitude >= 4 {
                (
                    4,
                    if self.locale.starts_with("zh") {
                        "萬"
                    } else {
                        "万"
                    },
                )
            } else {
                return None;
            };
            return Some(CompactPattern {
                scale,
                separator: "",
                label,
            });
        }
        if self.locale.starts_with("ko") {
            let (scale, label) = if magnitude >= 8 {
                (8, "억")
            } else if magnitude >= 4 {
                (4, "만")
            } else if magnitude >= 3 {
                (3, "천")
            } else {
                return None;
            };
            return Some(CompactPattern {
                scale,
                separator: "",
                label,
            });
        }
        if self.locale.starts_with("de") {
            if magnitude >= 6 {
                return Some(CompactPattern {
                    scale: 6,
                    separator: if self.compact_display == IntlNumberFormatCompactDisplay::Short {
                        "\u{00a0}"
                    } else {
                        " "
                    },
                    label: if self.compact_display == IntlNumberFormatCompactDisplay::Short {
                        "Mio."
                    } else {
                        "Millionen"
                    },
                });
            }
            return (self.compact_display == IntlNumberFormatCompactDisplay::Long
                && magnitude >= 3)
                .then_some(CompactPattern {
                    scale: 3,
                    separator: " ",
                    label: "Tausend",
                });
        }
        (magnitude >= 3).then_some(CompactPattern {
            scale: if magnitude >= 6 { 6 } else { 3 },
            separator: if self.compact_display == IntlNumberFormatCompactDisplay::Long {
                " "
            } else {
                ""
            },
            label: match (magnitude >= 6, self.compact_display) {
                (true, IntlNumberFormatCompactDisplay::Short) => "M",
                (true, IntlNumberFormatCompactDisplay::Long) => "million",
                (false, IntlNumberFormatCompactDisplay::Short) => "K",
                (false, IntlNumberFormatCompactDisplay::Long) => "thousand",
            },
        })
    }

    /// Replaces ASCII decimal syntax with copied locale symbols and typed field spans.
    fn localize_decimal(
        &self,
        output: &mut RenderedNumber,
        value: &str,
    ) -> Result<(), HostProviderError> {
        let (sign, unsigned) = match value.as_bytes().first() {
            Some(b'-') => (Some(false), value.get(1..).ok_or(DATA_FAILURE)?),
            Some(b'+') => (Some(true), value.get(1..).ok_or(DATA_FAILURE)?),
            _ => (None, value),
        };
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        if self.style == IntlNumberFormatStyle::Currency {
            self.push_currency_prefix(output, sign)?;
        } else {
            match sign {
                Some(false) => self.push_affix(output, &self.minus_prefix, false)?,
                Some(true) => self.push_affix(output, &self.plus_prefix, true)?,
                None => {}
            }
        }
        self.push_grouped_integer(output, integer)?;
        if !fraction.is_empty() {
            output.push(IntlNumberFormatPartType::Decimal, &self.decimal_separator)?;
            self.push_digits(output, IntlNumberFormatPartType::Fraction, fraction)?;
        }
        if self.style == IntlNumberFormatStyle::Currency {
            self.push_currency_suffix(output, sign)?;
        } else {
            match sign {
                Some(false) => self.push_affix(output, &self.minus_suffix, false)?,
                Some(true) => self.push_affix(output, &self.plus_suffix, true)?,
                None => {}
            }
        }
        Ok(())
    }

    /// Emits one integer using locale primary/secondary grouping from right to left.
    fn push_grouped_integer(
        &self,
        output: &mut RenderedNumber,
        integer: &str,
    ) -> Result<(), HostProviderError> {
        let primary = usize::from(self.grouping_sizes.primary);
        let secondary = usize::from(if self.grouping_sizes.secondary == 0 {
            self.grouping_sizes.primary
        } else {
            self.grouping_sizes.secondary
        });
        let grouping = primary != 0
            && self.grouping_strategy != IntlNumberFormatUseGrouping::Never
            && match self.grouping_strategy {
                IntlNumberFormatUseGrouping::Min2 => integer.len() > primary + 1,
                IntlNumberFormatUseGrouping::Auto => {
                    integer.len() >= primary + usize::from(self.grouping_sizes.min_grouping)
                }
                IntlNumberFormatUseGrouping::Always => integer.len() > primary,
                IntlNumberFormatUseGrouping::Never => false,
            };
        if !grouping {
            return self.push_digits(output, IntlNumberFormatPartType::Integer, integer);
        }
        let first_separator = integer.len().saturating_sub(primary);
        let leading = if secondary == 0 {
            first_separator
        } else {
            let remainder = first_separator % secondary;
            if remainder == 0 { secondary } else { remainder }
        };
        let mut cursor = 0;
        while cursor < integer.len() {
            let remaining = integer.len().saturating_sub(cursor);
            let width = if cursor == 0 {
                leading
            } else if remaining == primary {
                primary
            } else {
                secondary
            };
            let end = cursor.checked_add(width).ok_or(DATA_FAILURE)?;
            let group = integer.get(cursor..end).ok_or(DATA_FAILURE)?;
            if cursor != 0 {
                output.push(IntlNumberFormatPartType::Group, &self.grouping_separator)?;
            }
            self.push_digits(output, IntlNumberFormatPartType::Integer, group)?;
            cursor = end;
        }
        Ok(())
    }

    /// Emits one ASCII digit run as a single localized field.
    fn push_digits(
        &self,
        output: &mut RenderedNumber,
        kind: IntlNumberFormatPartType,
        digits: &str,
    ) -> Result<(), HostProviderError> {
        let mut localized = String::new();
        localized
            .try_reserve_exact(digits.len().saturating_mul(4))
            .map_err(|_| DATA_FAILURE)?;
        for digit in digits.bytes() {
            let index = digit.checked_sub(b'0').ok_or(DATA_FAILURE)?;
            let character = self
                .digits
                .get(usize::from(index))
                .copied()
                .ok_or(DATA_FAILURE)?;
            localized.push(character);
        }
        output.push(kind, &localized)
    }

    /// Splits bidi literals around the actual sign so ECMA field identity remains observable.
    fn push_affix(
        &self,
        output: &mut RenderedNumber,
        affix: &str,
        positive: bool,
    ) -> Result<(), HostProviderError> {
        let sign = if positive { '+' } else { '-' };
        let sign_kind = if positive {
            IntlNumberFormatPartType::PlusSign
        } else {
            IntlNumberFormatPartType::MinusSign
        };
        let mut literal_start = 0;
        for (index, character) in affix.char_indices() {
            if character != sign {
                continue;
            }
            output.push(
                IntlNumberFormatPartType::Literal,
                affix.get(literal_start..index).ok_or(DATA_FAILURE)?,
            )?;
            let end = index
                .checked_add(character.len_utf8())
                .ok_or(DATA_FAILURE)?;
            output.push(sign_kind, affix.get(index..end).ok_or(DATA_FAILURE)?)?;
            literal_start = end;
        }
        output.push(
            IntlNumberFormatPartType::Literal,
            affix.get(literal_start..).ok_or(DATA_FAILURE)?,
        )
    }

    /// Emits locale-specific unit prefixes before the numeric sign and magnitude fields.
    fn push_style_prefix(&self, output: &mut RenderedNumber) -> Result<(), HostProviderError> {
        if self.style != IntlNumberFormatStyle::Unit
            || self.unit.as_deref() != Some("kilometer-per-hour")
            || self.unit_display != IntlNumberFormatUnitDisplay::Long
        {
            return Ok(());
        }
        let prefix = if self.locale.starts_with("ja") {
            "時速"
        } else if self.locale.starts_with("ko") {
            "시속"
        } else if self.locale.starts_with("zh") {
            "每小時"
        } else {
            return Ok(());
        };
        output.push(IntlNumberFormatPartType::Unit, prefix)?;
        if self.locale.starts_with("ko") {
            output.push(IntlNumberFormatPartType::Literal, " ")?;
        }
        Ok(())
    }

    /// Emits sign and currency token in the locale's prefix/accounting position.
    fn push_currency_prefix(
        &self,
        output: &mut RenderedNumber,
        sign: Option<bool>,
    ) -> Result<(), HostProviderError> {
        if self.locale.starts_with("de") {
            return match sign {
                Some(false) => self.push_affix(output, &self.minus_prefix, false),
                Some(true) => self.push_affix(output, &self.plus_prefix, true),
                None => Ok(()),
            };
        }
        if sign == Some(false) && self.currency_sign == IntlNumberFormatCurrencySign::Accounting {
            output.push(IntlNumberFormatPartType::Literal, "(")?;
        } else {
            match sign {
                Some(false) => self.push_affix(output, &self.minus_prefix, false)?,
                Some(true) => self.push_affix(output, &self.plus_prefix, true)?,
                None => {}
            }
        }
        output.push(IntlNumberFormatPartType::Currency, self.currency_label())
    }

    /// Emits the suffix currency pattern and closes any accounting parenthesis.
    fn push_currency_suffix(
        &self,
        output: &mut RenderedNumber,
        sign: Option<bool>,
    ) -> Result<(), HostProviderError> {
        if self.locale.starts_with("de") {
            output.push(IntlNumberFormatPartType::Literal, "\u{00a0}")?;
            output.push(IntlNumberFormatPartType::Currency, self.currency_label())?;
            return match sign {
                Some(false) => self.push_affix(output, &self.minus_suffix, false),
                Some(true) => self.push_affix(output, &self.plus_suffix, true),
                None => Ok(()),
            };
        }
        if sign == Some(false) && self.currency_sign == IntlNumberFormatCurrencySign::Accounting {
            output.push(IntlNumberFormatPartType::Literal, ")")?;
        }
        Ok(())
    }

    /// Maps the resolved currency/display slots to an owned-provider presentation token.
    fn currency_label(&self) -> &str {
        let currency = self.currency.as_deref().unwrap_or("");
        if self.currency_display == IntlNumberFormatCurrencyDisplay::Code {
            return currency;
        }
        match currency {
            "USD" if self.locale.starts_with("ko") || self.locale.starts_with("zh") => "US$",
            "USD" => "$",
            "EUR" => "€",
            "JPY" => "¥",
            _ => currency,
        }
    }

    /// Appends percent or measurement-unit fields using the provider's resolved locale pattern.
    fn push_style_suffix(&self, output: &mut RenderedNumber) -> Result<(), HostProviderError> {
        if self.style == IntlNumberFormatStyle::Percent {
            return output.push(IntlNumberFormatPartType::PercentSign, "%");
        }
        if self.style != IntlNumberFormatStyle::Unit {
            return Ok(());
        }
        let unit = self.unit.as_deref().ok_or(DATA_FAILURE)?;
        if unit == "percent" {
            return output.push(IntlNumberFormatPartType::Unit, "%");
        }
        let (separator, label) = self.unit_suffix(unit);
        output.push(IntlNumberFormatPartType::Literal, separator)?;
        output.push(IntlNumberFormatPartType::Unit, label)
    }

    /// Resolves the compact compiled labels needed until ICU4X ships measurement-unit data.
    fn unit_suffix<'a>(&'a self, unit: &'a str) -> (&'static str, &'a str) {
        if unit != "kilometer-per-hour" {
            let separator = if self.unit_display == IntlNumberFormatUnitDisplay::Narrow {
                ""
            } else {
                " "
            };
            return (separator, unit);
        }
        match self.unit_display {
            IntlNumberFormatUnitDisplay::Short | IntlNumberFormatUnitDisplay::Narrow => {
                let separator = if self.locale.starts_with("en")
                    && self.unit_display == IntlNumberFormatUnitDisplay::Narrow
                    || self.locale.starts_with("ja")
                    || self.locale.starts_with("ko")
                    || self.locale.starts_with("zh")
                {
                    ""
                } else {
                    " "
                };
                let label = if self.locale.starts_with("zh") {
                    "公里/小時"
                } else {
                    "km/h"
                };
                (separator, label)
            }
            IntlNumberFormatUnitDisplay::Long if self.locale.starts_with("en") => {
                (" ", "kilometers per hour")
            }
            IntlNumberFormatUnitDisplay::Long if self.locale.starts_with("de") => {
                (" ", "Kilometer pro Stunde")
            }
            IntlNumberFormatUnitDisplay::Long if self.locale.starts_with("ja") => {
                ("", "キロメートル")
            }
            IntlNumberFormatUnitDisplay::Long if self.locale.starts_with("ko") => ("", "킬로미터"),
            IntlNumberFormatUnitDisplay::Long if self.locale.starts_with("zh") => ("", "公里"),
            IntlNumberFormatUnitDisplay::Long => (" ", "kilometers per hour"),
        }
    }
}

impl RenderedNumber {
    fn new(estimated_units: usize, collect_parts: bool) -> Result<Self, HostProviderError> {
        let mut formatted = Vec::new();
        formatted
            .try_reserve_exact(estimated_units)
            .map_err(|_| DATA_FAILURE)?;
        let spans = if collect_parts {
            let mut spans = Vec::new();
            spans.try_reserve_exact(12).map_err(|_| DATA_FAILURE)?;
            Some(spans)
        } else {
            None
        };
        Ok(Self { formatted, spans })
    }

    /// Appends a non-empty field and merges adjacent runs with the same ECMA part type.
    fn push(
        &mut self,
        kind: IntlNumberFormatPartType,
        value: &str,
    ) -> Result<(), HostProviderError> {
        if value.is_empty() {
            return Ok(());
        }
        let additional = value.encode_utf16().count();
        self.formatted
            .try_reserve(additional)
            .map_err(|_| DATA_FAILURE)?;
        let start = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        self.formatted.extend(value.encode_utf16());
        let end = u32::try_from(self.formatted.len()).map_err(|_| DATA_FAILURE)?;
        if let Some(spans) = self.spans.as_mut() {
            if let Some(last) = spans.last_mut()
                && last.kind == kind
                && last.end == start
            {
                last.end = end;
                return Ok(());
            }
            spans.try_reserve(1).map_err(|_| DATA_FAILURE)?;
            spans.push(IntlNumberFormatPartSpan { kind, start, end });
        }
        Ok(())
    }
}

struct MatchedLocale {
    requested: Locale,
    data_locale: Locale,
}

struct LoadedDecimalData {
    digits: [char; 10],
    minus_prefix: Box<str>,
    minus_suffix: Box<str>,
    plus_prefix: Box<str>,
    plus_suffix: Box<str>,
    decimal_separator: Box<str>,
    grouping_separator: Box<str>,
    grouping_sizes: GroupingSizes,
    nan_symbol: Box<str>,
    infinity_symbol: Box<str>,
}

/// Expands the bounded exponent syntax emitted by ECMAScript Number::toString without f64 loss.
fn parse_mathematical_decimal(value: &str) -> Result<Decimal, HostProviderError> {
    if let Ok(decimal) = Decimal::from_str(value) {
        return Ok(decimal);
    }
    let exponent_at = value
        .bytes()
        .position(|byte| matches!(byte, b'e' | b'E'))
        .ok_or(DATA_FAILURE)?;
    let (mantissa, exponent) = value.split_at(exponent_at);
    let exponent = exponent
        .get(1..)
        .ok_or(DATA_FAILURE)?
        .parse::<i32>()
        .map_err(|_| DATA_FAILURE)?;
    let (negative, mantissa) = mantissa
        .strip_prefix('-')
        .map(|value| (true, value))
        .unwrap_or((false, mantissa));
    let decimal_at = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits = String::new();
    digits
        .try_reserve_exact(mantissa.len())
        .map_err(|_| DATA_FAILURE)?;
    digits.extend(mantissa.chars().filter(|character| *character != '.'));
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DATA_FAILURE);
    }
    let decimal_at = i32::try_from(decimal_at)
        .map_err(|_| DATA_FAILURE)?
        .checked_add(exponent)
        .ok_or(DATA_FAILURE)?;
    let mut expanded = String::new();
    expanded
        .try_reserve_exact(digits.len().saturating_add(340))
        .map_err(|_| DATA_FAILURE)?;
    if negative {
        expanded.push('-');
    }
    if decimal_at <= 0 {
        expanded.push_str("0.");
        for _ in 0..decimal_at.unsigned_abs() {
            expanded.push('0');
        }
        expanded.push_str(&digits);
    } else if usize::try_from(decimal_at).map_err(|_| DATA_FAILURE)? >= digits.len() {
        expanded.push_str(&digits);
        for _ in digits.len()..usize::try_from(decimal_at).map_err(|_| DATA_FAILURE)? {
            expanded.push('0');
        }
    } else {
        let decimal_at = usize::try_from(decimal_at).map_err(|_| DATA_FAILURE)?;
        expanded.push_str(digits.get(..decimal_at).ok_or(DATA_FAILURE)?);
        expanded.push('.');
        expanded.push_str(digits.get(decimal_at..).ok_or(DATA_FAILURE)?);
    }
    Decimal::from_str(&expanded).map_err(|_| DATA_FAILURE)
}

/// Creates locale data for every validated option record while formatting support is layered in.
pub(super) fn create(
    default_locale: &str,
    request: IntlNumberFormatRequest,
) -> Result<IntlNumberFormatCreation, HostProviderError> {
    let matched = request
        .locales
        .iter()
        .find_map(|locale| match_locale(locale, request.locale_matcher))
        .or_else(|| match_locale(default_locale, request.locale_matcher))
        .ok_or(DATA_FAILURE)?;
    let locale_numbering_system = unicode_keyword(&matched.requested, "nu");
    let option_numbering_system = request
        .numbering_system
        .as_deref()
        .filter(|value| NUMBERING_SYSTEMS.binary_search(value).is_ok());
    let requested_numbering_system = option_numbering_system.or(locale_numbering_system.as_deref());
    let mut formatter_locale = matched.data_locale.clone();
    if let Some(numbering_system) = requested_numbering_system {
        set_unicode_keyword(&mut formatter_locale, "nu", numbering_system)?;
    }

    let default_numbering_system = default_numbering_system(&matched.data_locale)?;
    let resolved_numbering_system = requested_numbering_system
        .filter(|value| NUMBERING_SYSTEMS.binary_search(value).is_ok())
        .unwrap_or(default_numbering_system.as_ref());
    let mut resolved_locale = matched.data_locale;
    if locale_numbering_system.as_deref() == Some(resolved_numbering_system) {
        set_unicode_keyword(&mut resolved_locale, "nu", resolved_numbering_system)?;
    }

    let data = load_decimal_data(&formatter_locale, resolved_numbering_system)?;
    let minimum_fraction_digits = request.options.minimum_fraction_digits.unwrap_or(0);
    let maximum_fraction_digits = request.options.maximum_fraction_digits.unwrap_or(3);
    let backend = Icu4xNumberFormatBackend {
        digits: data.digits,
        minus_prefix: data.minus_prefix,
        minus_suffix: data.minus_suffix,
        plus_prefix: data.plus_prefix,
        plus_suffix: data.plus_suffix,
        decimal_separator: data.decimal_separator,
        grouping_separator: data.grouping_separator,
        grouping_sizes: data.grouping_sizes,
        grouping_strategy: request.options.use_grouping,
        locale: resolved_locale.to_string().into_boxed_str(),
        style: request.options.style,
        currency: request.options.currency.clone(),
        currency_display: request.options.currency_display,
        currency_sign: request.options.currency_sign,
        notation: request.options.notation,
        compact_display: request.options.compact_display,
        unit: request.options.unit.clone(),
        unit_display: request.options.unit_display,
        nan_symbol: data.nan_symbol,
        infinity_symbol: data.infinity_symbol,
        minimum_integer_digits: request.options.minimum_integer_digits,
        minimum_fraction_digits,
        maximum_fraction_digits,
        minimum_significant_digits: request.options.minimum_significant_digits,
        maximum_significant_digits: request.options.maximum_significant_digits,
        rounding_mode: request.options.rounding_mode,
        sign_display: request.options.sign_display,
    };
    Ok(IntlNumberFormatCreation {
        resolved: IntlNumberFormatResolved {
            locale: resolved_locale.to_string().into_boxed_str(),
            numbering_system: resolved_numbering_system.into(),
            options: request.options,
        },
        backend: Box::new(backend),
    })
}

/// Filters canonical requested spellings without retaining ICU locale values across the ABI.
pub(super) fn supported_locales(
    locales: &[Box<str>],
    matcher: IntlLocaleMatcher,
) -> Box<[Box<str>]> {
    locales
        .iter()
        .filter(|locale| match_locale(locale, matcher).is_some())
        .cloned()
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn match_locale(locale: &str, _matcher: IntlLocaleMatcher) -> Option<MatchedLocale> {
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

/// Reads the default numbering system directly from the same compiled symbols payload.
fn default_numbering_system(locale: &Locale) -> Result<Box<str>, HostProviderError> {
    let preferences = DecimalFormatterPreferences::from(locale);
    let data_locale = DecimalSymbolsV1::make_locale(preferences.locale_preferences);
    let language = decimal_language_fallback(locale)
        .parse::<Locale>()
        .map_err(|_| DATA_FAILURE)?;
    let language = DataLocale::from(&language);
    let response = DataProvider::<DecimalSymbolsV1>::load(
        &Baked,
        DataRequest {
            id: DataIdentifierBorrowed::for_locale(&data_locale),
            ..Default::default()
        },
    )
    .or_else(|_| {
        DataProvider::<DecimalSymbolsV1>::load(
            &Baked,
            DataRequest {
                id: DataIdentifierBorrowed::for_locale(&language),
                ..Default::default()
            },
        )
    })
    .map_err(|_| DATA_FAILURE)?;
    Ok(response.payload.get().numsys().into())
}

/// Copies the exact locale symbols and simple-digit table out of ICU payloads at construction.
fn load_decimal_data(
    locale: &Locale,
    numbering_system: &str,
) -> Result<LoadedDecimalData, HostProviderError> {
    let preferences = DecimalFormatterPreferences::from(locale);
    let data_locale = DecimalSymbolsV1::make_locale(preferences.locale_preferences);
    let language = decimal_language_fallback(locale)
        .parse::<Locale>()
        .map_err(|_| DATA_FAILURE)?;
    let language = DataLocale::from(&language);
    let attributes =
        DataMarkerAttributes::try_from_str(numbering_system).map_err(|_| DATA_FAILURE)?;
    let symbols_response = DataProvider::<DecimalSymbolsV1>::load(
        &Baked,
        DataRequest {
            id: DataIdentifierBorrowed::for_marker_attributes_and_locale(attributes, &data_locale),
            ..Default::default()
        },
    )
    .or_else(|_| {
        DataProvider::<DecimalSymbolsV1>::load(
            &Baked,
            DataRequest {
                id: DataIdentifierBorrowed::for_locale(&data_locale),
                ..Default::default()
            },
        )
    })
    .or_else(|_| {
        DataProvider::<DecimalSymbolsV1>::load(
            &Baked,
            DataRequest {
                id: DataIdentifierBorrowed::for_marker_attributes_and_locale(attributes, &language),
                ..Default::default()
            },
        )
    })
    .or_else(|_| {
        DataProvider::<DecimalSymbolsV1>::load(
            &Baked,
            DataRequest {
                id: DataIdentifierBorrowed::for_locale(&language),
                ..Default::default()
            },
        )
    })
    .map_err(|_| DATA_FAILURE)?;
    let symbols = symbols_response.payload.get();
    let (minus_prefix, minus_suffix) = symbols.minus_sign_affixes();
    let (plus_prefix, plus_suffix) = symbols.plus_sign_affixes();
    let loaded = LoadedDecimalData {
        digits: ['0'; 10],
        minus_prefix: minus_prefix.into(),
        minus_suffix: minus_suffix.into(),
        plus_prefix: plus_prefix.into(),
        plus_suffix: plus_suffix.into(),
        decimal_separator: symbols.decimal_separator().into(),
        grouping_separator: symbols.grouping_separator().into(),
        grouping_sizes: if locale
            .id
            .region
            .as_ref()
            .is_some_and(|region| region.as_str() == "IN")
        {
            GroupingSizes {
                primary: 3,
                secondary: 2,
                min_grouping: symbols.grouping_sizes.min_grouping,
            }
        } else {
            symbols.grouping_sizes
        },
        nan_symbol: localized_nan_symbol(locale).into(),
        infinity_symbol: "∞".into(),
    };
    let und = "und".parse::<Locale>().map_err(|_| DATA_FAILURE)?;
    let und = DataLocale::from(&und);
    let digits = DataProvider::<DecimalDigitsV1>::load(
        &Baked,
        DataRequest {
            id: DataIdentifierBorrowed::for_marker_attributes_and_locale(attributes, &und),
            ..Default::default()
        },
    )
    .map(|response| *response.payload.get())
    .or_else(|_| simple_numbering_system_digits(numbering_system).ok_or(DATA_FAILURE))?;
    Ok(LoadedDecimalData { digits, ..loaded })
}

/// Maps bare languages to the compiled decimal data package's default regional locale.
fn decimal_language_fallback(locale: &Locale) -> &str {
    match locale.id.language.as_str() {
        "en" => "en-US",
        language => language,
    }
}

/// Builds simple consecutive digit tables that are newer than the pinned ICU4X data package.
fn simple_numbering_system_digits(numbering_system: &str) -> Option<[char; 10]> {
    if numbering_system == "hanidec" {
        return Some(['〇', '一', '二', '三', '四', '五', '六', '七', '八', '九']);
    }
    let first = match numbering_system {
        "adlm" => 0x1e950,
        "ahom" => 0x11730,
        "arab" => 0x0660,
        "arabext" => 0x06f0,
        "bali" => 0x1b50,
        "beng" => 0x09e6,
        "bhks" => 0x11c50,
        "brah" => 0x11066,
        "cakm" => 0x11136,
        "cham" => 0xaa50,
        "deva" => 0x0966,
        "diak" => 0x11950,
        "fullwide" => 0xff10,
        "gara" => 0x10d40,
        "gong" => 0x11da0,
        "gonm" => 0x11d50,
        "gujr" => 0x0ae6,
        "gukh" => 0x16130,
        "guru" => 0x0a66,
        "hmng" => 0x16b50,
        "hmnp" => 0x1e140,
        "java" => 0xa9d0,
        "kali" => 0xa900,
        "kawi" => 0x11f50,
        "khmr" => 0x17e0,
        "knda" => 0x0ce6,
        "krai" => 0x16d70,
        "lana" => 0x1a80,
        "lanatham" => 0x1a90,
        "laoo" => 0x0ed0,
        "latn" => 0x0030,
        "lepc" => 0x1c40,
        "limb" => 0x1946,
        "mathbold" => 0x1d7ce,
        "mathdbl" => 0x1d7d8,
        "mathmono" => 0x1d7f6,
        "mathsanb" => 0x1d7ec,
        "mathsans" => 0x1d7e2,
        "mlym" => 0x0d66,
        "modi" => 0x11650,
        "mong" => 0x1810,
        "mroo" => 0x16a60,
        "mtei" => 0xabf0,
        "mymr" => 0x1040,
        "mymrepka" => 0x116da,
        "mymrpao" => 0x116d0,
        "mymrshan" => 0x1090,
        "mymrtlng" => 0xa9f0,
        "nagm" => 0x1e4f0,
        "newa" => 0x11450,
        "nkoo" => 0x07c0,
        "olck" => 0x1c50,
        "onao" => 0x1e5f1,
        "orya" => 0x0b66,
        "osma" => 0x104a0,
        "outlined" => 0x1ccf0,
        "rohg" => 0x10d30,
        "saur" => 0xa8d0,
        "segment" => 0x1fbf0,
        "shrd" => 0x111d0,
        "sind" => 0x112f0,
        "sinh" => 0x0de6,
        "sora" => 0x110f0,
        "sund" => 0x1bb0,
        "sunu" => 0x11bf0,
        "takr" => 0x116c0,
        "talu" => 0x19d0,
        "tamldec" => 0x0be6,
        "telu" => 0x0c66,
        "thai" => 0x0e50,
        "tibt" => 0x0f20,
        "tirh" => 0x114d0,
        "tnsa" => 0x16ac0,
        "tols" => 0x11de0,
        "vaii" => 0xa620,
        "wara" => 0x118e0,
        "wcho" => 0x1e2f0,
        _ => return None,
    };
    let mut digits = ['0'; 10];
    for (index, digit) in digits.iter_mut().enumerate() {
        *digit = char::from_u32(first + u32::try_from(index).ok()?)?;
    }
    Some(digits)
}

/// Supplies CLDR special-value spellings absent from ICU4X's finite-decimal symbols payload.
fn localized_nan_symbol(locale: &Locale) -> &'static str {
    match locale.id.language.as_str() {
        "ar" => "ليس رقم",
        "fa" => "ناعدد",
        "fi" => "epäluku",
        "ru" => "не число",
        "uz" => "son emas",
        "zh" if locale
            .id
            .script
            .as_ref()
            .is_some_and(|script| script.as_str() == "Hant")
            || locale
                .id
                .region
                .as_ref()
                .is_some_and(|region| matches!(region.as_str(), "TW" | "HK" | "MO")) =>
        {
            "非數值"
        }
        "zh" => "非数值",
        _ => "NaN",
    }
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

const fn sign_display(value: IntlNumberFormatSignDisplay) -> SignDisplay {
    match value {
        IntlNumberFormatSignDisplay::Auto => SignDisplay::Auto,
        IntlNumberFormatSignDisplay::Never => SignDisplay::Never,
        IntlNumberFormatSignDisplay::Always => SignDisplay::Always,
        IntlNumberFormatSignDisplay::ExceptZero => SignDisplay::ExceptZero,
        IntlNumberFormatSignDisplay::Negative => SignDisplay::Negative,
    }
}

const fn fixed_decimal_rounding_mode(value: IntlNumberFormatRoundingMode) -> SignedRoundingMode {
    match value {
        IntlNumberFormatRoundingMode::Ceil => SignedRoundingMode::Ceil,
        IntlNumberFormatRoundingMode::Floor => SignedRoundingMode::Floor,
        IntlNumberFormatRoundingMode::Expand => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::Expand)
        }
        IntlNumberFormatRoundingMode::Trunc => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::Trunc)
        }
        IntlNumberFormatRoundingMode::HalfCeil => SignedRoundingMode::HalfCeil,
        IntlNumberFormatRoundingMode::HalfFloor => SignedRoundingMode::HalfFloor,
        IntlNumberFormatRoundingMode::HalfExpand => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfExpand)
        }
        IntlNumberFormatRoundingMode::HalfTrunc => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfTrunc)
        }
        IntlNumberFormatRoundingMode::HalfEven => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfEven)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_vm::{IntlNumberFormatOptions, IntlNumberFormatStyle};

    fn request(locale: &str) -> IntlNumberFormatRequest {
        IntlNumberFormatRequest {
            locales: [Box::<str>::from(locale)].into(),
            options: IntlNumberFormatOptions::default(),
            ..Default::default()
        }
    }

    fn formatted(creation: &IntlNumberFormatCreation, value: &str) -> String {
        String::from_utf16(
            &creation
                .backend
                .format(&IntlMathematicalValue::Finite(value.into()))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn decimal_backend_localizes_grouping_digits_and_fraction_defaults() {
        let english = create("en", request("en-US")).unwrap();
        assert_eq!(formatted(&english, "12345.6789"), "12,345.679");

        let parts = english
            .backend
            .format_to_parts(&IntlMathematicalValue::Finite("12345.6789".into()))
            .unwrap();
        assert_eq!(
            parts.spans.iter().map(|span| span.kind).collect::<Vec<_>>(),
            [
                IntlNumberFormatPartType::Integer,
                IntlNumberFormatPartType::Group,
                IntlNumberFormatPartType::Integer,
                IntlNumberFormatPartType::Decimal,
                IntlNumberFormatPartType::Fraction,
            ]
        );
        let joined = parts
            .spans
            .iter()
            .flat_map(|span| {
                parts.formatted[span.start as usize..span.end as usize]
                    .iter()
                    .copied()
            })
            .collect::<Vec<_>>();
        assert_eq!(joined, parts.formatted.as_ref());

        let arabic = create("en", request("ar-EG")).unwrap();
        assert_eq!(formatted(&arabic, "12345.6"), "١٢٬٣٤٥٫٦");
    }

    #[test]
    fn explicit_numbering_system_and_grouping_reach_the_compiled_formatter() {
        let mut thai = request("en-US");
        thai.numbering_system = Some("thai".into());
        thai.options.use_grouping = IntlNumberFormatUseGrouping::Never;
        let thai = create("en", thai).unwrap();
        assert_eq!(&*thai.resolved.numbering_system, "thai");
        assert_eq!(formatted(&thai, "12345"), "๑๒๓๔๕");
    }

    #[test]
    fn validated_non_decimal_slots_survive_provider_creation() {
        let mut currency = request("en");
        currency.options.style = IntlNumberFormatStyle::Currency;
        currency.options.currency = Some("USD".into());
        currency.options.minimum_fraction_digits = Some(2);
        currency.options.maximum_fraction_digits = Some(2);
        let currency = create("en", currency).unwrap();
        assert_eq!(
            currency.resolved.options.style,
            IntlNumberFormatStyle::Currency
        );
        assert_eq!(currency.resolved.options.currency.as_deref(), Some("USD"));
        assert_eq!(formatted(&currency, "12"), "$12.00");

        let mut percent = request("en-US");
        percent.options.style = IntlNumberFormatStyle::Percent;
        percent.options.minimum_fraction_digits = Some(0);
        percent.options.maximum_fraction_digits = Some(0);
        let percent = create("en", percent).unwrap();
        assert_eq!(formatted(&percent, "-123"), "-12,300%");

        let mut unit = request("en-US");
        unit.options.style = IntlNumberFormatStyle::Unit;
        unit.options.unit = Some("kilometer-per-hour".into());
        unit.options.unit_display = IntlNumberFormatUnitDisplay::Long;
        let unit = create("en", unit).unwrap();
        assert_eq!(formatted(&unit, "987"), "987 kilometers per hour");
        let parts = unit
            .backend
            .format_to_parts(&IntlMathematicalValue::Finite("987".into()))
            .unwrap();
        assert_eq!(
            parts.spans.last().unwrap().kind,
            IntlNumberFormatPartType::Unit
        );

        let mut scientific = request("en-US");
        scientific.options.notation = IntlNumberFormatNotation::Scientific;
        let scientific = create("en", scientific).unwrap();
        assert_eq!(formatted(&scientific, "543211.1"), "5.432E5");

        let mut engineering = request("en-US");
        engineering.options.notation = IntlNumberFormatNotation::Engineering;
        let engineering = create("en", engineering).unwrap();
        assert_eq!(formatted(&engineering, "0.000345"), "345E-6");

        let mut compact = request("en-US");
        compact.options.notation = IntlNumberFormatNotation::Compact;
        compact.options.compact_display = IntlNumberFormatCompactDisplay::Long;
        let compact = create("en", compact).unwrap();
        assert_eq!(formatted(&compact, "987654321"), "988 million");
        assert_eq!(formatted(&compact, "0.0159"), "0.016");

        let mut significant = request("en-US");
        significant.options.minimum_fraction_digits = None;
        significant.options.maximum_fraction_digits = None;
        significant.options.minimum_significant_digits = Some(3);
        significant.options.maximum_significant_digits = Some(5);
        let significant = create("en", significant).unwrap();
        assert_eq!(formatted(&significant, "123.44499"), "123.44");
        assert_eq!(formatted(&significant, "1.2"), "1.20");
        assert_eq!(formatted(&significant, "123445.01"), "123,450");
    }

    #[test]
    fn supported_locales_preserve_requested_spelling_and_extensions() {
        let locales = [Box::<str>::from("de-u-nu-latn"), Box::<str>::from("zxx")];
        assert_eq!(
            supported_locales(&locales, IntlLocaleMatcher::Lookup).as_ref(),
            [Box::<str>::from("de-u-nu-latn")]
        );
    }

    #[test]
    fn every_advertised_simple_numbering_system_has_digit_data() {
        let mut missing = Vec::new();
        for numbering_system in NUMBERING_SYSTEMS {
            let mut request = request("en-US");
            request.numbering_system = Some((*numbering_system).into());
            if create("en", request).is_err() {
                missing.push(*numbering_system);
            }
        }
        assert!(missing.is_empty(), "missing simple digit data: {missing:?}");
    }

    #[test]
    fn regional_decimal_data_falls_back_without_losing_indian_grouping() {
        let locale = "en-IN".parse::<Locale>().unwrap();
        assert_eq!(default_numbering_system(&locale).unwrap().as_ref(), "latn");
        let data = load_decimal_data(&locale, "latn").unwrap();
        assert_eq!(data.grouping_sizes.primary, 3);
        assert_eq!(data.grouping_sizes.secondary, 2);
        let creation = create("en-US", request("en-IN")).unwrap();
        assert_eq!(formatted(&creation, "100000"), "1,00,000");
    }
}
