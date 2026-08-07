//! ICU4X compiled-data implementation of the provider-neutral PluralRules ABI.

use fixed_decimal::{CompactDecimal, Decimal};
use icu_locale::Locale;
use icu_plurals::{
    PluralCategory, PluralRuleType, PluralRules, PluralRulesOptions, PluralRulesPreferences,
};
use tachyon_vm::{
    HostProviderError, IntlLocaleMatcher, IntlMathematicalValue, IntlNumberFormatNotation,
    IntlNumberFormatOptions, IntlPluralCategory, IntlPluralRuleType, IntlPluralRulesBackend,
    IntlPluralRulesCreation, IntlPluralRulesRequest, IntlPluralRulesResolved,
};

use crate::number_format::parse_mathematical_decimal;

const DATA_FAILURE: HostProviderError = HostProviderError::Failure(7);

struct Icu4xPluralRulesBackend {
    locale: Locale,
    rule_type: IntlPluralRuleType,
    options: IntlNumberFormatOptions,
    manx_cardinal: bool,
}

impl IntlPluralRulesBackend for Icu4xPluralRulesBackend {
    /// Applies the already validated ECMA digit options before asking CLDR for a category.
    fn select(
        &self,
        value: &IntlMathematicalValue,
    ) -> Result<IntlPluralCategory, HostProviderError> {
        let Some(decimal) = rounded_decimal(value, &self.options)? else {
            return Ok(IntlPluralCategory::Other);
        };
        if self.manx_cardinal {
            return Ok(manx_cardinal_category(&decimal));
        }
        let rules = plural_rules(&self.locale, self.rule_type).ok_or(DATA_FAILURE)?;
        let category = if self.options.notation == IntlNumberFormatNotation::Compact {
            let exponent = (*decimal.magnitude_range().end()).max(0);
            let exponent = u8::try_from(exponent).map_err(|_| DATA_FAILURE)?;
            rules.category_for(&CompactDecimal::from_significand_and_exponent(
                decimal, exponent,
            ))
        } else {
            rules.category_for(&decimal)
        };
        Ok(category_from_icu(category))
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

/// Resolves one locale, freezes scalar options, and owns the compiled plural payload.
pub(super) fn create(
    default_locale: &str,
    request: IntlPluralRulesRequest,
) -> Result<IntlPluralRulesCreation, HostProviderError> {
    let (locale, rules) = request
        .locales
        .iter()
        .find_map(|locale| create_rules(locale, request.rule_type))
        .or_else(|| create_rules(default_locale, request.rule_type))
        .ok_or(DATA_FAILURE)?;
    let manx_cardinal = is_manx_cardinal(&locale, request.rule_type);
    let categories = if manx_cardinal {
        Box::new([
            IntlPluralCategory::One,
            IntlPluralCategory::Two,
            IntlPluralCategory::Few,
            IntlPluralCategory::Many,
            IntlPluralCategory::Other,
        ])
    } else {
        categories(&rules)
    };
    let resolved = IntlPluralRulesResolved {
        locale: locale.to_string().into_boxed_str(),
        rule_type: request.rule_type,
        options: request.options.clone(),
        categories,
    };
    Ok(IntlPluralRulesCreation {
        resolved,
        backend: Box::new(Icu4xPluralRulesBackend {
            locale,
            rule_type: request.rule_type,
            options: request.options,
            manx_cardinal,
        }),
    })
}

/// Filters canonical requested spellings using actual compiled plural data availability.
pub(super) fn supported_locales(
    locales: &[Box<str>],
    _matcher: IntlLocaleMatcher,
) -> Box<[Box<str>]> {
    let mut supported = Vec::with_capacity(locales.len());
    supported.extend(
        locales
            .iter()
            .filter(|locale| create_rules(locale, IntlPluralRuleType::Cardinal).is_some())
            .cloned(),
    );
    supported.into_boxed_slice()
}

fn create_rules(locale: &str, rule_type: IntlPluralRuleType) -> Option<(Locale, PluralRules)> {
    let mut locale = locale.parse::<Locale>().ok()?;
    locale.extensions = Default::default();
    if matches!(locale.id.language.as_str(), "und" | "zxx") {
        return None;
    }
    let rules = plural_rules(&locale, rule_type)?;
    Some((locale, rules))
}

fn plural_rules(locale: &Locale, rule_type: IntlPluralRuleType) -> Option<PluralRules> {
    let preferences = PluralRulesPreferences::from(locale);
    let options = PluralRulesOptions::default().with_type(match rule_type {
        IntlPluralRuleType::Cardinal => PluralRuleType::Cardinal,
        IntlPluralRuleType::Ordinal => PluralRuleType::Ordinal,
    });
    PluralRules::try_new(preferences, options).ok()
}

#[inline(always)]
fn is_manx_cardinal(locale: &Locale, rule_type: IntlPluralRuleType) -> bool {
    rule_type == IntlPluralRuleType::Cardinal && locale.id.language.as_str() == "gv"
}

/// Applies the CLDR 47 Manx cardinal rules omitted by ICU4X's modern-coverage baked data.
fn manx_cardinal_category(decimal: &Decimal) -> IntlPluralCategory {
    let range = decimal.magnitude_range();
    if *range.start() < 0 {
        return IntlPluralCategory::Many;
    }
    let units = decimal.digit_at(0);
    if units == 1 {
        return IntlPluralCategory::One;
    }
    if units == 2 {
        return IntlPluralCategory::Two;
    }
    let modulo_hundred = decimal.digit_at(1) * 10 + units;
    if matches!(modulo_hundred, 0 | 20 | 40 | 60 | 80) {
        return IntlPluralCategory::Few;
    }
    IntlPluralCategory::Other
}

fn categories(rules: &PluralRules) -> Box<[IntlPluralCategory]> {
    const ORDER: [PluralCategory; 6] = [
        PluralCategory::Zero,
        PluralCategory::One,
        PluralCategory::Two,
        PluralCategory::Few,
        PluralCategory::Many,
        PluralCategory::Other,
    ];
    let mut result = Vec::with_capacity(ORDER.len());
    for category in ORDER {
        if rules.categories().any(|available| available == category) {
            result.push(category_from_icu(category));
        }
    }
    result.into_boxed_slice()
}

const fn category_from_icu(value: PluralCategory) -> IntlPluralCategory {
    match value {
        PluralCategory::Zero => IntlPluralCategory::Zero,
        PluralCategory::One => IntlPluralCategory::One,
        PluralCategory::Two => IntlPluralCategory::Two,
        PluralCategory::Few => IntlPluralCategory::Few,
        PluralCategory::Many => IntlPluralCategory::Many,
        PluralCategory::Other => IntlPluralCategory::Other,
    }
}

/// Mirrors the currently shared NumberFormat rounding substrate without localized rendering.
fn rounded_decimal(
    value: &IntlMathematicalValue,
    options: &IntlNumberFormatOptions,
) -> Result<Option<Decimal>, HostProviderError> {
    let mut decimal = match value {
        IntlMathematicalValue::Finite(value) => parse_mathematical_decimal(value)?,
        IntlMathematicalValue::NegativeZero => "-0".parse().map_err(|_| DATA_FAILURE)?,
        IntlMathematicalValue::PositiveInfinity
        | IntlMathematicalValue::NegativeInfinity
        | IntlMathematicalValue::NaN => return Ok(None),
    };
    if let Some(maximum) = options.maximum_significant_digits {
        let magnitude = decimal.nonzero_magnitude_start();
        decimal.round_with_mode(
            magnitude - i16::from(maximum) + 1,
            super::number_format::fixed_decimal_rounding_mode(options.rounding_mode),
        );
        if let Some(minimum) = options.minimum_significant_digits {
            let magnitude = decimal.nonzero_magnitude_start();
            decimal.pad_end(magnitude - i16::from(minimum) + 1);
        }
    } else {
        let maximum = options.maximum_fraction_digits.unwrap_or(3);
        decimal.round_with_mode(
            -i16::from(maximum),
            super::number_format::fixed_decimal_rounding_mode(options.rounding_mode),
        );
        decimal.pad_end(-i16::from(options.minimum_fraction_digits.unwrap_or(0)));
    }
    decimal.pad_start(i16::from(options.minimum_integer_digits));
    Ok(Some(decimal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinal_ordinal_and_compact_categories_use_compiled_data() {
        let cardinal = create(
            "en-US",
            IntlPluralRulesRequest {
                locales: vec![Box::<str>::from("en-US")].into_boxed_slice(),
                locale_matcher: IntlLocaleMatcher::BestFit,
                rule_type: IntlPluralRuleType::Cardinal,
                options: IntlNumberFormatOptions::default(),
            },
        )
        .unwrap();
        assert_eq!(
            cardinal
                .backend
                .select(&IntlMathematicalValue::Finite("1".into()))
                .unwrap(),
            IntlPluralCategory::One
        );
        assert!(
            cardinal
                .resolved
                .categories
                .contains(&IntlPluralCategory::Other)
        );

        let ordinal = create(
            "en-US",
            IntlPluralRulesRequest {
                locales: vec![Box::<str>::from("en-US")].into_boxed_slice(),
                locale_matcher: IntlLocaleMatcher::BestFit,
                rule_type: IntlPluralRuleType::Ordinal,
                options: IntlNumberFormatOptions::default(),
            },
        )
        .unwrap();
        assert_eq!(
            ordinal
                .backend
                .select(&IntlMathematicalValue::Finite("2".into()))
                .unwrap(),
            IntlPluralCategory::Two
        );
    }

    #[test]
    fn test262_cardinal_category_order_matches_ecma_402() {
        let cases: &[(&str, &[IntlPluralCategory])] = &[
            (
                "ar",
                &[
                    IntlPluralCategory::Zero,
                    IntlPluralCategory::One,
                    IntlPluralCategory::Two,
                    IntlPluralCategory::Few,
                    IntlPluralCategory::Many,
                    IntlPluralCategory::Other,
                ],
            ),
            ("en", &[IntlPluralCategory::One, IntlPluralCategory::Other]),
            ("fa", &[IntlPluralCategory::One, IntlPluralCategory::Other]),
            (
                "fr",
                &[
                    IntlPluralCategory::One,
                    IntlPluralCategory::Many,
                    IntlPluralCategory::Other,
                ],
            ),
            (
                "gv",
                &[
                    IntlPluralCategory::One,
                    IntlPluralCategory::Two,
                    IntlPluralCategory::Few,
                    IntlPluralCategory::Many,
                    IntlPluralCategory::Other,
                ],
            ),
            ("ko", &[IntlPluralCategory::Other]),
            (
                "sl",
                &[
                    IntlPluralCategory::One,
                    IntlPluralCategory::Two,
                    IntlPluralCategory::Few,
                    IntlPluralCategory::Other,
                ],
            ),
        ];
        for (locale, expected) in cases {
            let creation = create(
                "en-US",
                IntlPluralRulesRequest {
                    locales: vec![Box::<str>::from(*locale)].into_boxed_slice(),
                    locale_matcher: IntlLocaleMatcher::BestFit,
                    rule_type: IntlPluralRuleType::Cardinal,
                    options: IntlNumberFormatOptions::default(),
                },
            )
            .unwrap();
            assert_eq!(creation.resolved.categories.as_ref(), *expected, "{locale}");
        }
    }
}
