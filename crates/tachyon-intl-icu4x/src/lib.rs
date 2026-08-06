#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! ICU4X-backed locale-data provider for Tachyon's executor-neutral Intl boundary.

use icu_locale::{
    Locale, LocaleCanonicalizer,
    extensions::transform::{Key as TransformKey, Value as TransformValue},
    extensions::unicode::{Key, Value},
};
use tachyon_vm::{HostProviderError, IntlProvider};

/// Extension aliases required by UTS 35 but absent from ICU4X 2.0's locale canonicalizer.
///
/// This table mirrors the alias cases pinned by Test262. Keeping the compatibility data in the
/// adapter lets the VM-facing provider contract remain stable when the workspace can move to an
/// ICU4X release whose compiled CLDR data performs these replacements itself.
const UNICODE_EXTENSION_ALIASES: &[UnicodeExtensionAlias] = &[
    UnicodeExtensionAlias::replace("ca", "ethiopic-amete-alem", "ethioaa"),
    UnicodeExtensionAlias::replace("ca", "islamicc", "islamic-civil"),
    UnicodeExtensionAlias::replace("ks", "primary", "level1"),
    UnicodeExtensionAlias::replace("ks", "tertiary", "level3"),
    UnicodeExtensionAlias::replace("ms", "imperial", "uksystem"),
    UnicodeExtensionAlias::replace("tz", "cnckg", "cnsha"),
    UnicodeExtensionAlias::replace("tz", "eire", "iedub"),
    UnicodeExtensionAlias::replace("tz", "est", "papty"),
    UnicodeExtensionAlias::replace("tz", "gmt0", "gmt"),
    UnicodeExtensionAlias::replace("tz", "uct", "utc"),
    UnicodeExtensionAlias::replace("tz", "zulu", "utc"),
    UnicodeExtensionAlias::replace("kb", "yes", "true"),
    UnicodeExtensionAlias::replace("kc", "yes", "true"),
    UnicodeExtensionAlias::replace("kh", "yes", "true"),
    UnicodeExtensionAlias::replace("kk", "yes", "true"),
    UnicodeExtensionAlias::replace("kn", "yes", "true"),
];

#[derive(Clone, Copy)]
struct UnicodeExtensionAlias {
    key: &'static str,
    alias: &'static str,
    replacement: &'static str,
}

impl UnicodeExtensionAlias {
    const fn replace(key: &'static str, alias: &'static str, replacement: &'static str) -> Self {
        Self {
            key,
            alias,
            replacement,
        }
    }
}

/// Locale provider using ICU4X compiled data without runtime filesystem access.
pub struct Icu4xIntlProvider {
    canonicalizer: LocaleCanonicalizer,
    default_locale: Box<str>,
}

impl Icu4xIntlProvider {
    /// Creates a provider after validating and canonicalizing its embedder-selected default locale.
    pub fn try_new(default_locale: &str) -> Result<Self, InvalidDefaultLocale> {
        let canonicalizer = LocaleCanonicalizer::new_extended();
        let mut locale = default_locale
            .parse::<Locale>()
            .map_err(|_| InvalidDefaultLocale)?;
        canonicalizer.canonicalize(&mut locale);
        canonicalize_unicode_extension_aliases(&mut locale);
        Ok(Self {
            canonicalizer,
            default_locale: locale.to_string().into_boxed_str(),
        })
    }
}

impl IntlProvider for Icu4xIntlProvider {
    fn canonicalize_locale(&mut self, locale: &str) -> Result<Option<Box<str>>, HostProviderError> {
        if locale.eq_ignore_ascii_case("posix") {
            return Ok(Some("posix".into()));
        }
        let Ok(mut locale) = locale.parse::<Locale>() else {
            return Ok(None);
        };
        self.canonicalizer.canonicalize(&mut locale);
        canonicalize_unicode_extension_aliases(&mut locale);
        canonicalize_transform_extension_aliases(&mut locale);
        Ok(Some(locale.to_string().into_boxed_str()))
    }

    fn default_locale(&mut self) -> Result<Box<str>, HostProviderError> {
        Ok(self.default_locale.clone())
    }
}

/// Invalid default locale supplied by an embedder during provider construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidDefaultLocale;

/// Applies the CLDR extension-value aliases not represented in ICU4X 2.0 compiled data.
fn canonicalize_unicode_extension_aliases(locale: &mut Locale) {
    let keywords = &mut locale.extensions.unicode.keywords;
    for alias in UNICODE_EXTENSION_ALIASES {
        let key = alias
            .key
            .parse::<Key>()
            .expect("static Unicode extension key must be valid");
        let expected = alias
            .alias
            .parse::<Value>()
            .expect("static Unicode extension alias must be valid");
        if keywords.get(&key) != Some(&expected) {
            continue;
        }
        let replacement = alias
            .replacement
            .parse::<Value>()
            .expect("static Unicode extension replacement must be valid");
        keywords.set(key, replacement);
    }
}

/// Applies the pinned CLDR transformed-extension value aliases missing from ICU4X 2.0.
fn canonicalize_transform_extension_aliases(locale: &mut Locale) {
    let key = "m0"
        .parse::<TransformKey>()
        .expect("static transformed extension key must be valid");
    let alias = "names"
        .parse::<TransformValue>()
        .expect("static transformed extension alias must be valid");
    if locale.extensions.transform.fields.get(&key) != Some(&alias) {
        return;
    }
    let replacement = "prprname"
        .parse::<TransformValue>()
        .expect("static transformed extension replacement must be valid");
    locale.extensions.transform.fields.set(key, replacement);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_aliases_and_unicode_extensions() {
        let mut provider = Icu4xIntlProvider::try_new("EN-us").unwrap();
        assert_eq!(&*provider.default_locale().unwrap(), "en-US");
        assert_eq!(
            provider
                .canonicalize_locale("cmn-hans-cn")
                .unwrap()
                .as_deref(),
            Some("zh-Hans-CN")
        );
        assert_eq!(
            provider
                .canonicalize_locale("de-DD-u-ca-islamicc")
                .unwrap()
                .as_deref(),
            Some("de-DE-u-ca-islamic-civil")
        );
    }

    #[test]
    fn canonicalizes_all_pinned_unicode_extension_alias_classes() {
        let cases = [
            ("und-u-ca-ethiopic-amete-alem", "und-u-ca-ethioaa"),
            ("und-u-ks-primary", "und-u-ks-level1"),
            ("und-u-ms-imperial", "und-u-ms-uksystem"),
            ("und-u-rg-cn11", "und-u-rg-cnbj"),
            ("und-CN-u-sd-cn11", "und-CN-u-sd-cnbj"),
            ("und-u-tz-zulu", "und-u-tz-utc"),
            ("und-u-kn-yes", "und-u-kn"),
            ("und-u-ka-yes", "und-u-ka-yes"),
        ];
        let mut provider = Icu4xIntlProvider::try_new("en-US").unwrap();
        for (input, expected) in cases {
            assert_eq!(
                provider.canonicalize_locale(input).unwrap().as_deref(),
                Some(expected),
                "input: {input}"
            );
        }
    }

    #[test]
    fn rejects_structurally_invalid_language_tags() {
        let mut provider = Icu4xIntlProvider::try_new("en-US").unwrap();
        assert_eq!(provider.canonicalize_locale("en_US").unwrap(), None);
        assert_eq!(provider.canonicalize_locale("x").unwrap(), None);
    }

    #[test]
    fn accepts_pinned_non_iana_and_transform_extension_forms() {
        let cases = [
            ("mo", "ro"),
            ("es-ES-preeuro", "es-ES-preeuro"),
            ("uz-UZ-cyrillic", "uz-UZ-cyrillic"),
            ("posix", "posix"),
            ("hi-direct", "hi-direct"),
            ("zh-pinyin", "zh-pinyin"),
            ("zh-stroke", "zh-stroke"),
            ("aar-x-private", "aa-x-private"),
            ("heb-x-private", "he-x-private"),
            ("ces", "cs"),
            ("hy-arevela", "hy"),
            ("hy-arevmda", "hyw"),
            ("sl-t-sl-rozaj-biske-1994", "sl-t-sl-1994-biske-rozaj"),
            ("DE-T-M0-DIN-K0-QWERTZ", "de-t-k0-qwertz-m0-din"),
            ("en-t-m0-true", "en-t-m0-true"),
            ("en-t-iw", "en-t-he"),
            (
                "und-Latn-t-und-hani-m0-names",
                "und-Latn-t-und-hani-m0-prprname",
            ),
        ];
        let mut provider = Icu4xIntlProvider::try_new("en-US").unwrap();
        let mut failures = Vec::new();
        for (input, expected) in cases {
            let actual = provider.canonicalize_locale(input).unwrap();
            if actual.as_deref() != Some(expected) {
                failures.push((input, expected, actual));
            }
        }
        assert!(failures.is_empty(), "mismatches: {failures:?}");
    }
}
