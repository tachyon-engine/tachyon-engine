//! Scalar parsing and digit normalization for the PluralRules option machine.

use super::*;

/// Returns the next observable option read in ECMA-402 constructor order.
pub(super) fn next_plural_rules_option(
    stage: IntlPluralRulesStage,
) -> Result<IntlPluralRulesStage, ExecutionError> {
    Ok(match stage {
        IntlPluralRulesStage::LocaleMatcher => IntlPluralRulesStage::Type,
        IntlPluralRulesStage::Type => IntlPluralRulesStage::Notation,
        IntlPluralRulesStage::Notation => IntlPluralRulesStage::CompactDisplay,
        IntlPluralRulesStage::CompactDisplay => IntlPluralRulesStage::MinimumIntegerDigits,
        IntlPluralRulesStage::MinimumIntegerDigits => IntlPluralRulesStage::MinimumFractionDigits,
        IntlPluralRulesStage::MinimumFractionDigits => IntlPluralRulesStage::MaximumFractionDigits,
        IntlPluralRulesStage::MaximumFractionDigits => {
            IntlPluralRulesStage::MinimumSignificantDigits
        }
        IntlPluralRulesStage::MinimumSignificantDigits => {
            IntlPluralRulesStage::MaximumSignificantDigits
        }
        IntlPluralRulesStage::MaximumSignificantDigits => IntlPluralRulesStage::RoundingIncrement,
        IntlPluralRulesStage::RoundingIncrement => IntlPluralRulesStage::RoundingMode,
        IntlPluralRulesStage::RoundingMode => IntlPluralRulesStage::RoundingPriority,
        IntlPluralRulesStage::RoundingPriority => IntlPluralRulesStage::TrailingZeroDisplay,
        _ => return Err(ExecutionError::MissingNativeContinuation),
    })
}

/// Maps an option stage to its exact JavaScript property spelling.
pub(super) fn plural_rules_option_name(
    stage: IntlPluralRulesStage,
) -> Result<&'static [u8], ExecutionError> {
    Ok(match stage {
        IntlPluralRulesStage::LocaleMatcher => b"localeMatcher",
        IntlPluralRulesStage::Type => b"type",
        IntlPluralRulesStage::Notation => b"notation",
        IntlPluralRulesStage::CompactDisplay => b"compactDisplay",
        IntlPluralRulesStage::MinimumIntegerDigits => b"minimumIntegerDigits",
        IntlPluralRulesStage::MinimumFractionDigits => b"minimumFractionDigits",
        IntlPluralRulesStage::MaximumFractionDigits => b"maximumFractionDigits",
        IntlPluralRulesStage::MinimumSignificantDigits => b"minimumSignificantDigits",
        IntlPluralRulesStage::MaximumSignificantDigits => b"maximumSignificantDigits",
        IntlPluralRulesStage::RoundingIncrement => b"roundingIncrement",
        IntlPluralRulesStage::RoundingMode => b"roundingMode",
        IntlPluralRulesStage::RoundingPriority => b"roundingPriority",
        IntlPluralRulesStage::TrailingZeroDisplay => b"trailingZeroDisplay",
        _ => return Err(ExecutionError::MissingNativeContinuation),
    })
}

/// Validates the closed string domain selected by one GetOption call.
pub(super) fn plural_rules_valid_string_option(stage: IntlPluralRulesStage, value: &str) -> bool {
    match stage {
        IntlPluralRulesStage::LocaleMatcher => matches!(value, "lookup" | "best fit"),
        IntlPluralRulesStage::Type => matches!(value, "cardinal" | "ordinal"),
        IntlPluralRulesStage::Notation => {
            matches!(value, "standard" | "scientific" | "engineering" | "compact")
        }
        IntlPluralRulesStage::CompactDisplay => matches!(value, "short" | "long"),
        IntlPluralRulesStage::RoundingMode => matches!(
            value,
            "ceil"
                | "floor"
                | "expand"
                | "trunc"
                | "halfCeil"
                | "halfFloor"
                | "halfExpand"
                | "halfTrunc"
                | "halfEven"
        ),
        IntlPluralRulesStage::RoundingPriority => {
            matches!(value, "auto" | "morePrecision" | "lessPrecision")
        }
        IntlPluralRulesStage::TrailingZeroDisplay => {
            matches!(value, "auto" | "stripIfInteger")
        }
        _ => false,
    }
}

#[inline(always)]
pub(super) fn plural_rules_raw_digit_stage(stage: IntlPluralRulesStage) -> bool {
    matches!(
        stage,
        IntlPluralRulesStage::MinimumFractionDigits
            | IntlPluralRulesStage::MaximumFractionDigits
            | IntlPluralRulesStage::MinimumSignificantDigits
            | IntlPluralRulesStage::MaximumSignificantDigits
    )
}

#[inline(always)]
pub(super) fn plural_rules_numeric_stage(stage: IntlPluralRulesStage) -> bool {
    matches!(
        stage,
        IntlPluralRulesStage::MinimumIntegerDigits | IntlPluralRulesStage::RoundingIncrement
    )
}

#[inline(always)]
pub(super) fn plural_rules_digit_conversion_stage(stage: IntlPluralRulesStage) -> bool {
    matches!(
        stage,
        IntlPluralRulesStage::ConvertMinimumSignificantDigits
            | IntlPluralRulesStage::ConvertMaximumSignificantDigits
            | IntlPluralRulesStage::ConvertMinimumFractionDigits
            | IntlPluralRulesStage::ConvertMaximumFractionDigits
    )
}

pub(super) fn plural_rules_raw_digit_value(
    snapshot: PendingIntlPluralRules,
    stage: IntlPluralRulesStage,
) -> Result<Value, ExecutionError> {
    match stage {
        IntlPluralRulesStage::ConvertMinimumSignificantDigits => {
            Ok(snapshot.minimum_significant_raw)
        }
        IntlPluralRulesStage::ConvertMaximumSignificantDigits => {
            Ok(snapshot.maximum_significant_raw)
        }
        IntlPluralRulesStage::ConvertMinimumFractionDigits => Ok(snapshot.minimum_fraction_raw),
        IntlPluralRulesStage::ConvertMaximumFractionDigits => Ok(snapshot.maximum_fraction_raw),
        _ => Err(ExecutionError::MissingNativeContinuation),
    }
}

pub(super) fn plural_rules_numeric_range(
    stage: IntlPluralRulesStage,
) -> Result<(f64, f64), ExecutionError> {
    match stage {
        IntlPluralRulesStage::MinimumIntegerDigits => Ok((1.0, 21.0)),
        IntlPluralRulesStage::RoundingIncrement => Ok((1.0, 5000.0)),
        IntlPluralRulesStage::ConvertMinimumSignificantDigits
        | IntlPluralRulesStage::ConvertMaximumSignificantDigits => Ok((1.0, 21.0)),
        IntlPluralRulesStage::ConvertMinimumFractionDigits
        | IntlPluralRulesStage::ConvertMaximumFractionDigits => Ok((0.0, 100.0)),
        _ => Err(ExecutionError::MissingNativeContinuation),
    }
}

pub(super) fn normalize_plural_rules_significant_digits(
    snapshot: PendingIntlPluralRules,
) -> Result<(Option<u8>, Option<u8>), ExecutionError> {
    if !snapshot.need_significant {
        return Ok((None, None));
    }
    let minimum = snapshot.minimum_significant_digits.unwrap_or(1);
    let maximum = snapshot.maximum_significant_digits.unwrap_or(21);
    if minimum > maximum {
        return Err(ExecutionError::InvalidIntlPluralRulesOption);
    }
    Ok((Some(minimum), Some(maximum)))
}

/// Applies PluralRules fraction defaults and validates minimum/maximum ordering.
pub(super) fn normalize_plural_rules_fraction_digits(
    snapshot: PendingIntlPluralRules,
) -> Result<(Option<u8>, Option<u8>), ExecutionError> {
    if !snapshot.need_fraction {
        return Ok(if snapshot.need_significant {
            (None, None)
        } else {
            (Some(0), Some(0))
        });
    }
    let default_minimum = 0;
    let default_maximum = if snapshot.rounding_increment == 1 {
        3
    } else {
        0
    };
    let (minimum, maximum) = match (
        snapshot.minimum_fraction_digits,
        snapshot.maximum_fraction_digits,
    ) {
        (Some(minimum), Some(maximum)) if minimum > maximum => {
            return Err(ExecutionError::InvalidIntlPluralRulesOption);
        }
        (Some(minimum), Some(maximum)) => (minimum, maximum),
        (Some(minimum), None) => (minimum, default_maximum.max(minimum)),
        (None, Some(maximum)) => (default_minimum.min(maximum), maximum),
        (None, None) => (default_minimum, default_maximum),
    };
    Ok((Some(minimum), Some(maximum)))
}

#[inline(always)]
pub(super) fn valid_plural_rules_rounding_increment(value: u16) -> bool {
    matches!(
        value,
        1 | 2 | 5 | 10 | 20 | 25 | 50 | 100 | 200 | 250 | 500 | 1000 | 2000 | 2500 | 5000
    )
}

#[inline(always)]
pub(super) const fn intl_plural_category_name(category: IntlPluralCategory) -> &'static [u8] {
    match category {
        IntlPluralCategory::Zero => b"zero",
        IntlPluralCategory::One => b"one",
        IntlPluralCategory::Two => b"two",
        IntlPluralCategory::Few => b"few",
        IntlPluralCategory::Many => b"many",
        IntlPluralCategory::Other => b"other",
    }
}

#[inline(always)]
pub(super) const fn intl_plural_rules_notation_name(
    notation: IntlNumberFormatNotation,
) -> &'static [u8] {
    match notation {
        IntlNumberFormatNotation::Standard => b"standard",
        IntlNumberFormatNotation::Scientific => b"scientific",
        IntlNumberFormatNotation::Engineering => b"engineering",
        IntlNumberFormatNotation::Compact => b"compact",
    }
}

pub(super) const fn intl_plural_rules_rounding_mode_name(
    mode: IntlNumberFormatRoundingMode,
) -> &'static [u8] {
    match mode {
        IntlNumberFormatRoundingMode::Ceil => b"ceil",
        IntlNumberFormatRoundingMode::Floor => b"floor",
        IntlNumberFormatRoundingMode::Expand => b"expand",
        IntlNumberFormatRoundingMode::Trunc => b"trunc",
        IntlNumberFormatRoundingMode::HalfCeil => b"halfCeil",
        IntlNumberFormatRoundingMode::HalfFloor => b"halfFloor",
        IntlNumberFormatRoundingMode::HalfExpand => b"halfExpand",
        IntlNumberFormatRoundingMode::HalfTrunc => b"halfTrunc",
        IntlNumberFormatRoundingMode::HalfEven => b"halfEven",
    }
}

pub(super) const fn intl_plural_rules_rounding_priority_name(
    priority: IntlNumberFormatRoundingPriority,
) -> &'static [u8] {
    match priority {
        IntlNumberFormatRoundingPriority::Auto => b"auto",
        IntlNumberFormatRoundingPriority::MorePrecision => b"morePrecision",
        IntlNumberFormatRoundingPriority::LessPrecision => b"lessPrecision",
    }
}

pub(super) const fn intl_plural_rules_trailing_zero_name(
    display: IntlNumberFormatTrailingZeroDisplay,
) -> &'static [u8] {
    match display {
        IntlNumberFormatTrailingZeroDisplay::Auto => b"auto",
        IntlNumberFormatTrailingZeroDisplay::StripIfInteger => b"stripIfInteger",
    }
}
