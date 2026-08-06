//! Scalar parsing and validation helpers for the NumberFormat option state machine.

use super::*;

impl Isolate {
    pub(super) fn set_number_format_locale_matcher(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let matcher = match value {
            "lookup" => IntlLocaleMatcher::Lookup,
            "best fit" => IntlLocaleMatcher::BestFit,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.locale_matcher = matcher)
    }

    pub(super) fn set_number_format_style(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let style = match value {
            "decimal" => IntlNumberFormatStyle::Decimal,
            "percent" => IntlNumberFormatStyle::Percent,
            "currency" => IntlNumberFormatStyle::Currency,
            "unit" => IntlNumberFormatStyle::Unit,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.style = style)
    }

    pub(super) fn set_number_format_currency_display(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let display = match value {
            "code" => IntlNumberFormatCurrencyDisplay::Code,
            "symbol" => IntlNumberFormatCurrencyDisplay::Symbol,
            "narrowSymbol" => IntlNumberFormatCurrencyDisplay::NarrowSymbol,
            "name" => IntlNumberFormatCurrencyDisplay::Name,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.currency_display = display)
    }

    pub(super) fn set_number_format_currency_sign(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let sign = match value {
            "standard" => IntlNumberFormatCurrencySign::Standard,
            "accounting" => IntlNumberFormatCurrencySign::Accounting,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.currency_sign = sign)
    }

    pub(super) fn set_number_format_unit_display(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let display = match value {
            "short" => IntlNumberFormatUnitDisplay::Short,
            "narrow" => IntlNumberFormatUnitDisplay::Narrow,
            "long" => IntlNumberFormatUnitDisplay::Long,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.unit_display = display)
    }

    pub(super) fn set_number_format_notation(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let notation = match value {
            "standard" => IntlNumberFormatNotation::Standard,
            "scientific" => IntlNumberFormatNotation::Scientific,
            "engineering" => IntlNumberFormatNotation::Engineering,
            "compact" => IntlNumberFormatNotation::Compact,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.notation = notation)
    }

    pub(super) fn set_number_format_rounding_mode(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let mode = match value {
            "ceil" => IntlNumberFormatRoundingMode::Ceil,
            "floor" => IntlNumberFormatRoundingMode::Floor,
            "expand" => IntlNumberFormatRoundingMode::Expand,
            "trunc" => IntlNumberFormatRoundingMode::Trunc,
            "halfCeil" => IntlNumberFormatRoundingMode::HalfCeil,
            "halfFloor" => IntlNumberFormatRoundingMode::HalfFloor,
            "halfExpand" => IntlNumberFormatRoundingMode::HalfExpand,
            "halfTrunc" => IntlNumberFormatRoundingMode::HalfTrunc,
            "halfEven" => IntlNumberFormatRoundingMode::HalfEven,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.rounding_mode = mode)
    }

    pub(super) fn set_number_format_rounding_priority(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let priority = match value {
            "auto" => IntlNumberFormatRoundingPriority::Auto,
            "morePrecision" => IntlNumberFormatRoundingPriority::MorePrecision,
            "lessPrecision" => IntlNumberFormatRoundingPriority::LessPrecision,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| {
            pending.rounding_priority = priority
        })
    }

    pub(super) fn set_number_format_trailing_zero_display(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let display = match value {
            "auto" => IntlNumberFormatTrailingZeroDisplay::Auto,
            "stripIfInteger" => IntlNumberFormatTrailingZeroDisplay::StripIfInteger,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| {
            pending.trailing_zero_display = display
        })
    }

    pub(super) fn set_number_format_compact_display(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let display = match value {
            "short" => IntlNumberFormatCompactDisplay::Short,
            "long" => IntlNumberFormatCompactDisplay::Long,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.compact_display = display)
    }

    pub(super) fn set_number_format_grouping(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        let default = if snapshot.notation == IntlNumberFormatNotation::Compact {
            IntlNumberFormatUseGrouping::Min2
        } else {
            IntlNumberFormatUseGrouping::Auto
        };
        let grouping = match value {
            "min2" => IntlNumberFormatUseGrouping::Min2,
            "auto" => IntlNumberFormatUseGrouping::Auto,
            "always" => IntlNumberFormatUseGrouping::Always,
            "true" | "false" => default,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.use_grouping = grouping)
    }

    pub(super) fn set_number_format_sign_display(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        value: &str,
    ) -> Result<(), ExecutionError> {
        let display = match value {
            "auto" => IntlNumberFormatSignDisplay::Auto,
            "never" => IntlNumberFormatSignDisplay::Never,
            "always" => IntlNumberFormatSignDisplay::Always,
            "exceptZero" => IntlNumberFormatSignDisplay::ExceptZero,
            "negative" => IntlNumberFormatSignDisplay::Negative,
            _ => return Err(ExecutionError::InvalidIntlNumberFormatOption),
        };
        self.update_pending_intl_number_format(state, |pending| pending.sign_display = display)
    }
}

pub(super) fn native_site(site: &CallSite) -> NativeContinuationSite {
    NativeContinuationSite {
        caller_base: site.caller_base,
        destination: site.destination,
        call_site: site.call_site,
    }
}

pub(super) fn next_number_format_option(
    stage: IntlNumberFormatStage,
) -> Result<IntlNumberFormatStage, ExecutionError> {
    Ok(match stage {
        IntlNumberFormatStage::LocaleMatcher => IntlNumberFormatStage::NumberingSystem,
        IntlNumberFormatStage::NumberingSystem => IntlNumberFormatStage::Style,
        IntlNumberFormatStage::Style => IntlNumberFormatStage::Currency,
        IntlNumberFormatStage::Currency => IntlNumberFormatStage::CurrencyDisplay,
        IntlNumberFormatStage::CurrencyDisplay => IntlNumberFormatStage::CurrencySign,
        IntlNumberFormatStage::CurrencySign => IntlNumberFormatStage::Unit,
        IntlNumberFormatStage::Unit => IntlNumberFormatStage::UnitDisplay,
        IntlNumberFormatStage::UnitDisplay => IntlNumberFormatStage::Notation,
        IntlNumberFormatStage::Notation => IntlNumberFormatStage::MinimumIntegerDigits,
        IntlNumberFormatStage::MinimumIntegerDigits => IntlNumberFormatStage::MinimumFractionDigits,
        IntlNumberFormatStage::MinimumFractionDigits => {
            IntlNumberFormatStage::MaximumFractionDigits
        }
        IntlNumberFormatStage::MaximumFractionDigits => {
            IntlNumberFormatStage::MinimumSignificantDigits
        }
        IntlNumberFormatStage::MinimumSignificantDigits => {
            IntlNumberFormatStage::MaximumSignificantDigits
        }
        IntlNumberFormatStage::MaximumSignificantDigits => IntlNumberFormatStage::RoundingIncrement,
        IntlNumberFormatStage::RoundingIncrement => IntlNumberFormatStage::RoundingMode,
        IntlNumberFormatStage::RoundingMode => IntlNumberFormatStage::RoundingPriority,
        IntlNumberFormatStage::RoundingPriority => IntlNumberFormatStage::TrailingZeroDisplay,
        IntlNumberFormatStage::TrailingZeroDisplay => IntlNumberFormatStage::CompactDisplay,
        IntlNumberFormatStage::CompactDisplay => IntlNumberFormatStage::UseGrouping,
        IntlNumberFormatStage::UseGrouping => IntlNumberFormatStage::SignDisplay,
        IntlNumberFormatStage::SignDisplay => IntlNumberFormatStage::SignDisplay,
        _ => return Err(ExecutionError::MissingNativeContinuation),
    })
}

pub(super) fn number_format_option_name(
    stage: IntlNumberFormatStage,
) -> Result<&'static [u8], ExecutionError> {
    Ok(match stage {
        IntlNumberFormatStage::LocaleMatcher => b"localeMatcher",
        IntlNumberFormatStage::NumberingSystem => b"numberingSystem",
        IntlNumberFormatStage::Style => b"style",
        IntlNumberFormatStage::Currency => b"currency",
        IntlNumberFormatStage::CurrencyDisplay => b"currencyDisplay",
        IntlNumberFormatStage::CurrencySign => b"currencySign",
        IntlNumberFormatStage::Unit => b"unit",
        IntlNumberFormatStage::UnitDisplay => b"unitDisplay",
        IntlNumberFormatStage::Notation => b"notation",
        IntlNumberFormatStage::MinimumIntegerDigits => b"minimumIntegerDigits",
        IntlNumberFormatStage::MinimumFractionDigits => b"minimumFractionDigits",
        IntlNumberFormatStage::MaximumFractionDigits => b"maximumFractionDigits",
        IntlNumberFormatStage::MinimumSignificantDigits => b"minimumSignificantDigits",
        IntlNumberFormatStage::MaximumSignificantDigits => b"maximumSignificantDigits",
        IntlNumberFormatStage::RoundingIncrement => b"roundingIncrement",
        IntlNumberFormatStage::RoundingMode => b"roundingMode",
        IntlNumberFormatStage::RoundingPriority => b"roundingPriority",
        IntlNumberFormatStage::TrailingZeroDisplay => b"trailingZeroDisplay",
        IntlNumberFormatStage::CompactDisplay => b"compactDisplay",
        IntlNumberFormatStage::UseGrouping => b"useGrouping",
        IntlNumberFormatStage::SignDisplay => b"signDisplay",
        _ => return Err(ExecutionError::MissingNativeContinuation),
    })
}

pub(super) fn is_raw_digit_stage(stage: IntlNumberFormatStage) -> bool {
    matches!(
        stage,
        IntlNumberFormatStage::MinimumFractionDigits
            | IntlNumberFormatStage::MaximumFractionDigits
            | IntlNumberFormatStage::MinimumSignificantDigits
            | IntlNumberFormatStage::MaximumSignificantDigits
    )
}

pub(super) fn is_immediate_numeric_stage(stage: IntlNumberFormatStage) -> bool {
    matches!(
        stage,
        IntlNumberFormatStage::MinimumIntegerDigits | IntlNumberFormatStage::RoundingIncrement
    )
}

pub(super) fn is_digit_conversion_stage(stage: IntlNumberFormatStage) -> bool {
    matches!(
        stage,
        IntlNumberFormatStage::ConvertMinimumSignificantDigits
            | IntlNumberFormatStage::ConvertMaximumSignificantDigits
            | IntlNumberFormatStage::ConvertMinimumFractionDigits
            | IntlNumberFormatStage::ConvertMaximumFractionDigits
    )
}

pub(super) fn is_numeric_conversion_stage(stage: IntlNumberFormatStage) -> bool {
    is_immediate_numeric_stage(stage) || is_digit_conversion_stage(stage)
}

pub(super) fn raw_digit_value(
    snapshot: PendingIntlNumberFormat,
    stage: IntlNumberFormatStage,
) -> Result<Value, ExecutionError> {
    match stage {
        IntlNumberFormatStage::ConvertMinimumSignificantDigits => {
            Ok(snapshot.minimum_significant_raw)
        }
        IntlNumberFormatStage::ConvertMaximumSignificantDigits => {
            Ok(snapshot.maximum_significant_raw)
        }
        IntlNumberFormatStage::ConvertMinimumFractionDigits => Ok(snapshot.minimum_fraction_raw),
        IntlNumberFormatStage::ConvertMaximumFractionDigits => Ok(snapshot.maximum_fraction_raw),
        _ => Err(ExecutionError::MissingNativeContinuation),
    }
}

pub(super) fn numeric_option_range(
    stage: IntlNumberFormatStage,
) -> Result<(f64, f64), ExecutionError> {
    match stage {
        IntlNumberFormatStage::MinimumIntegerDigits => Ok((1.0, 21.0)),
        IntlNumberFormatStage::RoundingIncrement => Ok((1.0, 5000.0)),
        IntlNumberFormatStage::ConvertMinimumSignificantDigits
        | IntlNumberFormatStage::ConvertMaximumSignificantDigits => Ok((1.0, 21.0)),
        IntlNumberFormatStage::ConvertMinimumFractionDigits
        | IntlNumberFormatStage::ConvertMaximumFractionDigits => Ok((0.0, 100.0)),
        _ => Err(ExecutionError::MissingNativeContinuation),
    }
}

pub(super) fn normalize_significant_digits(
    snapshot: PendingIntlNumberFormat,
) -> Result<(Option<u8>, Option<u8>), ExecutionError> {
    if !snapshot.need_significant {
        return Ok((None, None));
    }
    let minimum = snapshot.minimum_significant_digits.unwrap_or(1);
    let maximum = snapshot.maximum_significant_digits.unwrap_or(21);
    if minimum > maximum {
        return Err(ExecutionError::InvalidIntlNumberFormatOption);
    }
    Ok((Some(minimum), Some(maximum)))
}

pub(super) fn normalize_fraction_digits(
    snapshot: PendingIntlNumberFormat,
    default_minimum: u8,
    default_maximum: u8,
) -> Result<(Option<u8>, Option<u8>), ExecutionError> {
    if !snapshot.need_fraction {
        return Ok(if snapshot.need_significant {
            (None, None)
        } else {
            (Some(0), Some(0))
        });
    }
    let (minimum, maximum) = match (
        snapshot.minimum_fraction_digits,
        snapshot.maximum_fraction_digits,
    ) {
        (Some(minimum), Some(maximum)) if minimum > maximum => {
            return Err(ExecutionError::InvalidIntlNumberFormatOption);
        }
        (Some(minimum), Some(maximum)) => (minimum, maximum),
        (Some(minimum), None) => (minimum, default_maximum.max(minimum)),
        (None, Some(maximum)) => (default_minimum.min(maximum), maximum),
        (None, None) => (default_minimum, default_maximum),
    };
    Ok((Some(minimum), Some(maximum)))
}

pub(super) fn fraction_defaults(style: IntlNumberFormatStyle) -> (u8, u8) {
    match style {
        IntlNumberFormatStyle::Percent => (0, 0),
        IntlNumberFormatStyle::Currency => (2, 2),
        IntlNumberFormatStyle::Decimal | IntlNumberFormatStyle::Unit => (0, 3),
    }
}

pub(super) fn is_rounding_increment(value: u16) -> bool {
    matches!(
        value,
        1 | 2 | 5 | 10 | 20 | 25 | 50 | 100 | 200 | 250 | 500 | 1000 | 2000 | 2500 | 5000
    )
}

pub(super) fn normalize_currency(value: &str) -> Result<[u8; 3], ExecutionError> {
    let bytes = value.as_bytes();
    if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_alphabetic) {
        return Err(ExecutionError::InvalidIntlNumberFormatOption);
    }
    Ok([
        bytes[0].to_ascii_uppercase(),
        bytes[1].to_ascii_uppercase(),
        bytes[2].to_ascii_uppercase(),
    ])
}

pub(super) fn is_unicode_locale_type(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|subtag| {
            (3..=8).contains(&subtag.len())
                && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

pub(super) fn is_well_formed_unit(value: &str) -> bool {
    const UNITS: [&str; 45] = [
        "acre",
        "bit",
        "byte",
        "celsius",
        "centimeter",
        "day",
        "degree",
        "fahrenheit",
        "fluid-ounce",
        "foot",
        "gallon",
        "gigabit",
        "gigabyte",
        "gram",
        "hectare",
        "hour",
        "inch",
        "kilobit",
        "kilobyte",
        "kilogram",
        "kilometer",
        "liter",
        "megabit",
        "megabyte",
        "meter",
        "microsecond",
        "mile",
        "mile-scandinavian",
        "milliliter",
        "millimeter",
        "millisecond",
        "minute",
        "month",
        "nanosecond",
        "ounce",
        "percent",
        "petabyte",
        "pound",
        "second",
        "stone",
        "terabit",
        "terabyte",
        "week",
        "yard",
        "year",
    ];
    let (numerator, denominator) = value
        .split_once("-per-")
        .filter(|(_, denominator)| !denominator.is_empty())
        .unwrap_or((value, ""));
    UNITS.binary_search(&numerator).is_ok()
        && (denominator.is_empty() || UNITS.binary_search(&denominator).is_ok())
}
