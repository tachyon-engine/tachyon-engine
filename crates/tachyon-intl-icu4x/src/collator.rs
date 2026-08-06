//! ICU4X compiled-data implementation of Tachyon's provider-neutral Collator ABI.

use core::cmp::Ordering;

use icu_collator::{
    CollatorBorrowed, CollatorPreferences,
    options::{AlternateHandling, CaseLevel, CollatorOptions, MaxVariable, Strength},
    preferences::{CollationCaseFirst, CollationNumericOrdering, CollationType},
    provider::{Baked, CollationMetadataV1},
};
use icu_locale::{
    Locale,
    extensions::unicode::{Key, Value},
};
use icu_provider::{
    DataIdentifierBorrowed, DataMarkerAttributes, DataProvider, DataRequest, marker::DataMarkerExt,
};
use tachyon_vm::{
    HostProviderError, IntlCollatorBackend, IntlCollatorCaseFirst, IntlCollatorCreation,
    IntlCollatorRequest, IntlCollatorResolved, IntlCollatorSensitivity, IntlCollatorUsage,
    IntlLocaleMatcher,
};

use crate::supported_values::COLLATIONS;

const DATA_FAILURE: HostProviderError = HostProviderError::Failure(1);

/// Static ICU payload view retained by one initialized ECMAScript Collator.
struct Icu4xCollatorBackend {
    collator: CollatorBorrowed<'static>,
}

impl IntlCollatorBackend for Icu4xCollatorBackend {
    #[inline]
    fn compare_utf16(&self, left: &[u16], right: &[u16]) -> Result<Ordering, HostProviderError> {
        Ok(self.collator.compare_utf16(left, right))
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

/// One matched request together with its extension-free ICU comparison locale.
struct MatchedLocale {
    requested: Locale,
    data_locale: Locale,
}

/// Creates one cached borrowed collator and the exact slots observable by ECMAScript.
pub(super) fn create(
    default_locale: &str,
    request: IntlCollatorRequest,
) -> Result<IntlCollatorCreation, HostProviderError> {
    let matched = request
        .locales
        .iter()
        .find_map(|locale| match_locale(locale, request.locale_matcher))
        .or_else(|| match_locale(default_locale, request.locale_matcher))
        .ok_or(DATA_FAILURE)?;
    let locale_preferences = CollatorPreferences::from(&matched.requested);
    let locale_collation = locale_preferences
        .collation_type
        .filter(|collation| usable_collation(*collation, &matched.data_locale));
    let option_collation = request
        .collation
        .as_deref()
        .and_then(parse_collation)
        .filter(|collation| usable_collation(*collation, &matched.data_locale));
    let selected_collation = option_collation.or(locale_collation);

    let selected_numeric = request.numeric.or_else(|| {
        locale_preferences
            .numeric_ordering
            .map(|value| value == CollationNumericOrdering::True)
    });
    let selected_case_first = request
        .case_first
        .or_else(|| locale_preferences.case_first.map(case_first_from_icu));
    let mut preferences = CollatorPreferences::from(&matched.data_locale);
    preferences.collation_type = match request.usage {
        IntlCollatorUsage::Sort => selected_collation,
        IntlCollatorUsage::Search if matched.data_locale.id.language.as_str() == "de" => {
            Some(CollationType::Phonebk)
        }
        IntlCollatorUsage::Search => Some(CollationType::Search),
    };
    preferences.numeric_ordering = selected_numeric.map(numeric_to_icu);
    preferences.case_first = selected_case_first.map(case_first_to_icu);

    let resolved_sensitivity = request.sensitivity.unwrap_or(match request.usage {
        IntlCollatorUsage::Sort => IntlCollatorSensitivity::Variant,
        IntlCollatorUsage::Search => IntlCollatorSensitivity::Base,
    });
    let options = collator_options(Some(resolved_sensitivity), request.ignore_punctuation);
    let collator = CollatorBorrowed::try_new(preferences, options).map_err(|_| DATA_FAILURE)?;
    let icu_resolved = collator.resolved_options();
    let resolved_locale = resolved_locale(
        matched,
        locale_collation,
        option_collation,
        selected_numeric,
        request.numeric,
        selected_case_first,
        request.case_first,
    );
    let resolved = IntlCollatorResolved {
        locale: resolved_locale.to_string().into_boxed_str(),
        usage: request.usage,
        sensitivity: resolved_sensitivity,
        ignore_punctuation: icu_resolved.alternate_handling == AlternateHandling::Shifted
            && icu_resolved.max_variable != MaxVariable::Space,
        collation: selected_collation
            .map(|value| Box::<str>::from(value.as_str()))
            .unwrap_or_else(|| Box::<str>::from("default")),
        numeric: icu_resolved.numeric == CollationNumericOrdering::True,
        case_first: case_first_from_icu(icu_resolved.case_first),
    };
    Ok(IntlCollatorCreation {
        resolved,
        backend: Box::new(Icu4xCollatorBackend { collator }),
    })
}

/// Returns canonical requested spellings for linguistically meaningful locale identifiers.
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

/// Matches one canonical request while excluding locale identifiers with no linguistic content.
fn match_locale(locale: &str, _matcher: IntlLocaleMatcher) -> Option<MatchedLocale> {
    let requested = locale.parse::<Locale>().ok()?;
    let mut base = requested.clone();
    base.extensions = Default::default();
    let language = base.id.language.as_str();
    if matches!(language, "und" | "zxx") {
        return None;
    }
    Some(MatchedLocale {
        requested,
        data_locale: base,
    })
}

/// Accepts only collations advertised by this adapter and backed for the selected locale.
fn usable_collation(collation: CollationType, locale: &Locale) -> bool {
    let name = collation.as_str();
    if matches!(collation, CollationType::Search | CollationType::Standard)
        || COLLATIONS.binary_search(&name).is_err()
    {
        return false;
    }
    let preferences = CollatorPreferences::from(locale);
    let data_locale = CollationMetadataV1::make_locale(preferences.locale_preferences);
    let attributes = DataMarkerAttributes::try_from_str(name)
        .expect("ICU CollationType must contain valid marker attributes");
    DataProvider::<CollationMetadataV1>::load(
        &Baked,
        DataRequest {
            id: DataIdentifierBorrowed::for_marker_attributes_and_locale(attributes, &data_locale),
            ..Default::default()
        },
    )
    .is_ok()
}

/// Builds the locale slot with only supported `co`, `kn`, and `kf` extension additions.
#[allow(clippy::too_many_arguments)]
fn resolved_locale(
    matched: MatchedLocale,
    locale_collation: Option<CollationType>,
    option_collation: Option<CollationType>,
    numeric: Option<bool>,
    option_numeric: Option<bool>,
    case_first: Option<IntlCollatorCaseFirst>,
    option_case_first: Option<IntlCollatorCaseFirst>,
) -> Locale {
    let locale_preferences = CollatorPreferences::from(&matched.requested);
    let mut resolved = matched.data_locale;
    if let Some(collation) =
        locale_collation.filter(|value| option_collation.is_none_or(|option| option == *value))
    {
        set_unicode_keyword(&mut resolved, "co", collation.as_str());
    }
    let locale_numeric = locale_preferences
        .numeric_ordering
        .map(|value| value == CollationNumericOrdering::True);
    if locale_numeric == numeric
        && option_numeric.is_none_or(|option| Some(option) == locale_numeric)
        && let Some(value) = locale_numeric
    {
        set_unicode_keyword(&mut resolved, "kn", if value { "true" } else { "false" });
    }
    let locale_case_first = locale_preferences.case_first.map(case_first_from_icu);
    if locale_case_first == case_first
        && option_case_first.is_none_or(|option| Some(option) == locale_case_first)
        && let Some(value) = locale_case_first
    {
        set_unicode_keyword(&mut resolved, "kf", case_first_name(value));
    }
    resolved
}

/// Sets one already validated static Unicode extension key/value pair.
fn set_unicode_keyword(locale: &mut Locale, key: &str, value: &str) {
    let key = key
        .parse::<Key>()
        .expect("static Unicode extension key must be valid");
    let value = value
        .parse::<Value>()
        .expect("resolved Unicode extension value must be valid");
    locale.extensions.unicode.keywords.set(key, value);
}

/// Maps ECMAScript sensitivity and punctuation controls onto ICU comparison options.
fn collator_options(
    sensitivity: Option<IntlCollatorSensitivity>,
    ignore_punctuation: Option<bool>,
) -> CollatorOptions {
    let mut options = CollatorOptions::default();
    if let Some(sensitivity) = sensitivity {
        let (strength, case_level) = match sensitivity {
            IntlCollatorSensitivity::Base => (Strength::Primary, CaseLevel::Off),
            IntlCollatorSensitivity::Accent => (Strength::Secondary, CaseLevel::Off),
            IntlCollatorSensitivity::Case => (Strength::Primary, CaseLevel::On),
            IntlCollatorSensitivity::Variant => (Strength::Tertiary, CaseLevel::Off),
        };
        options.strength = Some(strength);
        options.case_level = Some(case_level);
    }
    if let Some(ignore) = ignore_punctuation {
        options.alternate_handling = Some(if ignore {
            AlternateHandling::Shifted
        } else {
            AlternateHandling::NonIgnorable
        });
        if ignore {
            options.max_variable = Some(MaxVariable::Punctuation);
        }
    }
    options
}

fn parse_collation(value: &str) -> Option<CollationType> {
    Some(match value {
        "compat" => CollationType::Compat,
        "dict" => CollationType::Dict,
        "ducet" => CollationType::Ducet,
        "emoji" => CollationType::Emoji,
        "eor" => CollationType::Eor,
        "phonebk" => CollationType::Phonebk,
        "phonetic" => CollationType::Phonetic,
        "pinyin" => CollationType::Pinyin,
        "search" => CollationType::Search,
        "searchjl" => CollationType::Searchjl,
        "standard" => CollationType::Standard,
        "stroke" => CollationType::Stroke,
        "trad" => CollationType::Trad,
        "unihan" => CollationType::Unihan,
        "zhuyin" => CollationType::Zhuyin,
        _ => return None,
    })
}

const fn numeric_to_icu(value: bool) -> CollationNumericOrdering {
    if value {
        CollationNumericOrdering::True
    } else {
        CollationNumericOrdering::False
    }
}

const fn case_first_to_icu(value: IntlCollatorCaseFirst) -> CollationCaseFirst {
    match value {
        IntlCollatorCaseFirst::Upper => CollationCaseFirst::Upper,
        IntlCollatorCaseFirst::Lower => CollationCaseFirst::Lower,
        IntlCollatorCaseFirst::False => CollationCaseFirst::False,
    }
}

const fn case_first_from_icu(value: CollationCaseFirst) -> IntlCollatorCaseFirst {
    match value {
        CollationCaseFirst::Upper => IntlCollatorCaseFirst::Upper,
        CollationCaseFirst::Lower => IntlCollatorCaseFirst::Lower,
        CollationCaseFirst::False => IntlCollatorCaseFirst::False,
        _ => IntlCollatorCaseFirst::False,
    }
}

const fn case_first_name(value: IntlCollatorCaseFirst) -> &'static str {
    match value {
        IntlCollatorCaseFirst::Upper => "upper",
        IntlCollatorCaseFirst::Lower => "lower",
        IntlCollatorCaseFirst::False => "false",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(locale: &str) -> IntlCollatorRequest {
        IntlCollatorRequest {
            locales: [Box::<str>::from(locale)].into(),
            ..Default::default()
        }
    }

    fn utf16(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    #[test]
    /// Proves that compiled static payload references satisfy the isolate handoff contract.
    fn borrowed_backend_is_send_and_owns_no_dynamic_backing() {
        fn assert_send<T: Send>() {}
        assert_send::<Icu4xCollatorBackend>();
        let creation = create("en", request("en")).unwrap();
        assert_eq!(creation.backend.external_memory_bytes(), 0);
    }

    #[test]
    /// Exercises canonical normalization and ill-formed UTF-16 directly at the backend boundary.
    fn cached_backend_compares_utf16_and_canonical_equivalents() {
        let creation = create("en", request("en")).unwrap();
        assert_eq!(
            creation
                .backend
                .compare_utf16(&utf16("o\u{308}"), &utf16("ö")),
            Ok(Ordering::Equal)
        );
        assert_eq!(
            creation.backend.compare_utf16(&[0xD800], &[0xD800]),
            Ok(Ordering::Equal)
        );
    }

    #[test]
    /// Checks that high-value ECMA-402 comparison options alter the cached ICU comparator.
    fn numeric_and_sensitivity_options_reach_compiled_collator() {
        let mut numeric = request("en");
        numeric.numeric = Some(true);
        let numeric = create("en", numeric).unwrap();
        assert_eq!(
            numeric.backend.compare_utf16(&utf16("2"), &utf16("10")),
            Ok(Ordering::Less)
        );

        let mut base = request("en");
        base.sensitivity = Some(IntlCollatorSensitivity::Base);
        let base = create("en", base).unwrap();
        assert_eq!(
            base.backend.compare_utf16(&utf16("A"), &utf16("a")),
            Ok(Ordering::Equal)
        );
    }

    #[test]
    /// Keeps the ECMA-402 German search tailoring distinct from ordinary sort ordering.
    fn german_search_and_sort_order_ae_and_a_umlaut_differently() {
        let sort = create("de", request("de")).unwrap();
        assert_eq!(
            sort.backend.compare_utf16(&utf16("AE"), &utf16("Ä")),
            Ok(Ordering::Greater)
        );
        let mut search = request("de");
        search.usage = IntlCollatorUsage::Search;
        let search = create("de", search).unwrap();
        assert_eq!(
            search.backend.compare_utf16(&utf16("AE"), &utf16("Ä")),
            Ok(Ordering::Equal)
        );
    }

    #[test]
    /// Keeps resolved punctuation state synchronized with the comparison implementation.
    fn punctuation_option_and_thai_default_match_resolved_backend() {
        let mut ignored = request("en");
        ignored.ignore_punctuation = Some(true);
        let ignored = create("en", ignored).unwrap();
        assert!(ignored.resolved.ignore_punctuation);
        assert_eq!(
            ignored.backend.compare_utf16(&[], &utf16("*")),
            Ok(Ordering::Equal)
        );
        assert!(
            create("en", request("th"))
                .unwrap()
                .resolved
                .ignore_punctuation
        );
    }

    #[test]
    /// Verifies locale-extension retention when explicit options agree or override.
    fn locale_extensions_and_explicit_options_follow_override_rules() {
        let locale = create("en", request("de-u-co-phonebk-kn-false")).unwrap();
        assert_eq!(&*locale.resolved.locale, "de-u-co-phonebk-kn-false");
        assert_eq!(&*locale.resolved.collation, "phonebk");

        let mut overridden = request("de-u-co-phonebk-kn-false");
        overridden.collation = Some("eor".into());
        overridden.numeric = Some(true);
        let overridden = create("en", overridden).unwrap();
        assert_eq!(&*overridden.resolved.locale, "de");
        assert_eq!(&*overridden.resolved.collation, "eor");
        assert!(overridden.resolved.numeric);
    }

    #[test]
    /// Preserves a supported locale extension when an explicit collation is unsupported.
    fn unsupported_option_does_not_displace_supported_locale_collation() {
        let mut request = request("de-u-co-phonebk");
        request.collation = Some("pinyin".into());
        let creation = create("en", request).unwrap();
        assert_eq!(&*creation.resolved.locale, "de-u-co-phonebk");
        assert_eq!(&*creation.resolved.collation, "phonebk");
    }

    #[test]
    /// Maps search usage and every scalar option without requiring dynamic ICU payloads.
    fn search_usage_and_scalar_slots_are_preserved() {
        let mut request = request("de");
        request.usage = IntlCollatorUsage::Search;
        request.case_first = Some(IntlCollatorCaseFirst::Upper);
        request.sensitivity = Some(IntlCollatorSensitivity::Accent);
        request.ignore_punctuation = Some(false);
        let creation = create("en", request).unwrap();
        assert_eq!(creation.resolved.usage, IntlCollatorUsage::Search);
        assert_eq!(
            creation.resolved.sensitivity,
            IntlCollatorSensitivity::Accent
        );
        assert_eq!(creation.resolved.case_first, IntlCollatorCaseFirst::Upper);
        assert!(!creation.resolved.ignore_punctuation);
    }

    #[test]
    /// Locks supportedValuesOf enumeration to capabilities constructible by this adapter.
    fn every_advertised_collation_is_accepted_by_one_test262_locale() {
        let cases = [
            ("compat", "ar"),
            ("dict", "si"),
            ("emoji", "en"),
            ("eor", "en"),
            ("phonebk", "de"),
            ("pinyin", "zh"),
            ("stroke", "zh"),
            ("trad", "es"),
            ("unihan", "zh"),
            ("zhuyin", "zh"),
        ];
        for (collation, locale) in cases {
            let mut request = request(locale);
            request.collation = Some(collation.into());
            let creation = create("en", request).unwrap();
            assert_eq!(&*creation.resolved.collation, collation);
        }
    }

    #[test]
    /// Filters without rewriting requested canonical locale strings or their extensions.
    fn supported_locales_preserves_requests_and_rejects_no_linguistic_content() {
        let locales = [
            Box::<str>::from("en-US-u-kn"),
            Box::<str>::from("zxx"),
            Box::<str>::from("tlh"),
            Box::<str>::from("de"),
        ];
        assert_eq!(
            supported_locales(&locales, IntlLocaleMatcher::Lookup).as_ref(),
            [
                Box::<str>::from("en-US-u-kn"),
                Box::<str>::from("tlh"),
                Box::<str>::from("de")
            ]
        );
    }
}
