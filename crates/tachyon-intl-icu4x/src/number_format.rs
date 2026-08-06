//! ICU4X compiled-data implementation of Tachyon's provider-neutral NumberFormat ABI.

use core::str::FromStr;

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
    HostProviderError, IntlLocaleMatcher, IntlMathematicalValue, IntlNumberFormatBackend,
    IntlNumberFormatCreation, IntlNumberFormatNotation, IntlNumberFormatRequest,
    IntlNumberFormatResolved, IntlNumberFormatSignDisplay, IntlNumberFormatStyle,
    IntlNumberFormatUseGrouping,
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
    minimum_integer_digits: u8,
    minimum_fraction_digits: u8,
    maximum_fraction_digits: u8,
    sign_display: IntlNumberFormatSignDisplay,
}

impl IntlNumberFormatBackend for Icu4xNumberFormatBackend {
    /// Converts exact decimal text only after the VM has completed observable ToNumeric work.
    fn format(&self, value: &IntlMathematicalValue) -> Result<Box<[u16]>, HostProviderError> {
        let formatted = match value {
            IntlMathematicalValue::Finite(value) => {
                let mut decimal = Decimal::from_str(value).map_err(|_| DATA_FAILURE)?;
                decimal.round(-i16::from(self.maximum_fraction_digits));
                decimal.pad_end(-i16::from(self.minimum_fraction_digits));
                decimal.pad_start(i16::from(self.minimum_integer_digits));
                decimal.apply_sign_display(sign_display(self.sign_display));
                self.localize_decimal(&decimal.to_string())?
            }
            IntlMathematicalValue::PositiveInfinity => match self.sign_display {
                IntlNumberFormatSignDisplay::Always | IntlNumberFormatSignDisplay::ExceptZero => {
                    "+∞".into()
                }
                _ => "∞".into(),
            },
            IntlMathematicalValue::NegativeInfinity => match self.sign_display {
                IntlNumberFormatSignDisplay::Never => "∞".into(),
                _ => "-∞".into(),
            },
            IntlMathematicalValue::NaN => "NaN".into(),
        };
        Ok(formatted
            .encode_utf16()
            .collect::<Vec<_>>()
            .into_boxed_slice())
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

impl Icu4xNumberFormatBackend {
    /// Replaces ASCII decimal syntax with the copied locale symbols and grouping pattern.
    fn localize_decimal(&self, value: &str) -> Result<String, HostProviderError> {
        let (sign, unsigned) = match value.as_bytes().first() {
            Some(b'-') => (Some(false), value.get(1..).ok_or(DATA_FAILURE)?),
            Some(b'+') => (Some(true), value.get(1..).ok_or(DATA_FAILURE)?),
            _ => (None, value),
        };
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let mut output = String::new();
        let estimated = value
            .len()
            .saturating_mul(4)
            .saturating_add(self.minus_prefix.len())
            .saturating_add(self.minus_suffix.len());
        output
            .try_reserve_exact(estimated)
            .map_err(|_| DATA_FAILURE)?;
        match sign {
            Some(false) => output.push_str(&self.minus_prefix),
            Some(true) => output.push_str(&self.plus_prefix),
            None => {}
        }
        self.push_grouped_integer(&mut output, integer)?;
        if !fraction.is_empty() {
            output.push_str(&self.decimal_separator);
            self.push_digits(&mut output, fraction)?;
        }
        match sign {
            Some(false) => output.push_str(&self.minus_suffix),
            Some(true) => output.push_str(&self.plus_suffix),
            None => {}
        }
        Ok(output)
    }

    /// Emits one integer using locale primary/secondary grouping from right to left.
    fn push_grouped_integer(
        &self,
        output: &mut String,
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
            return self.push_digits(output, integer);
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
            let width = if cursor == 0 { leading } else { secondary };
            let end = cursor.checked_add(width).ok_or(DATA_FAILURE)?;
            let group = integer.get(cursor..end).ok_or(DATA_FAILURE)?;
            if cursor != 0 {
                output.push_str(&self.grouping_separator);
            }
            self.push_digits(output, group)?;
            cursor = end;
        }
        Ok(())
    }

    fn push_digits(&self, output: &mut String, digits: &str) -> Result<(), HostProviderError> {
        for digit in digits.bytes() {
            let index = digit.checked_sub(b'0').ok_or(DATA_FAILURE)?;
            let localized = self
                .digits
                .get(usize::from(index))
                .copied()
                .ok_or(DATA_FAILURE)?;
            output.push(localized);
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
}

/// Creates the first standards-aligned decimal substrate without pretending unsupported styles work.
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
    if request.options.style != IntlNumberFormatStyle::Decimal
        || request.options.notation != IntlNumberFormatNotation::Standard
        || request.options.rounding_increment != 1
        || request.options.minimum_significant_digits.is_some()
        || request.options.maximum_significant_digits.is_some()
    {
        return Err(HostProviderError::Unavailable);
    }

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
    if option_numbering_system.is_none()
        && locale_numbering_system.as_deref() == Some(resolved_numbering_system)
    {
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
        minimum_integer_digits: request.options.minimum_integer_digits,
        minimum_fraction_digits,
        maximum_fraction_digits,
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
    let response = DataProvider::<DecimalSymbolsV1>::load(
        &Baked,
        DataRequest {
            id: DataIdentifierBorrowed::for_locale(&data_locale),
            ..Default::default()
        },
    )
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
        grouping_sizes: symbols.grouping_sizes,
    };
    let und = "und".parse::<Locale>().map_err(|_| DATA_FAILURE)?;
    let und = DataLocale::from(&und);
    let digits_response = DataProvider::<DecimalDigitsV1>::load(
        &Baked,
        DataRequest {
            id: DataIdentifierBorrowed::for_marker_attributes_and_locale(attributes, &und),
            ..Default::default()
        },
    )
    .map_err(|_| DATA_FAILURE)?;
    Ok(LoadedDecimalData {
        digits: *digits_response.payload.get(),
        ..loaded
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_vm::IntlNumberFormatOptions;

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
    fn unsupported_style_is_explicit_instead_of_silently_formatting_as_decimal() {
        let mut currency = request("en");
        currency.options.style = IntlNumberFormatStyle::Currency;
        assert!(matches!(
            create("en", currency),
            Err(HostProviderError::Unavailable)
        ));
    }

    #[test]
    fn supported_locales_preserve_requested_spelling_and_extensions() {
        let locales = [Box::<str>::from("de-u-nu-latn"), Box::<str>::from("zxx")];
        assert_eq!(
            supported_locales(&locales, IntlLocaleMatcher::Lookup).as_ref(),
            [Box::<str>::from("de-u-nu-latn")]
        );
    }
}
