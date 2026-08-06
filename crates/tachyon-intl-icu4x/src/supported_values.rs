//! Owned ECMA-402 capability enumeration backed by ICU data and pinned specification tables.

use icu_time::zone::iana::IanaParserExtended;
use tachyon_vm::IntlSupportedValuesKey;

/// Returns one owned capability list so the VM never borrows adapter or ICU storage across GC.
pub(super) fn supported_values(key: IntlSupportedValuesKey) -> Box<[Box<str>]> {
    let values = match key {
        IntlSupportedValuesKey::Calendar => CALENDARS,
        IntlSupportedValuesKey::Collation => COLLATIONS,
        IntlSupportedValuesKey::Currency => CURRENCIES,
        IntlSupportedValuesKey::NumberingSystem => NUMBERING_SYSTEMS,
        IntlSupportedValuesKey::TimeZone => return supported_time_zones(),
        IntlSupportedValuesKey::Unit => UNITS,
    };
    values
        .iter()
        .map(|value| Box::<str>::from(*value))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Enumerates canonical primary IANA identifiers and applies ECMA-402's UTC spelling.
fn supported_time_zones() -> Box<[Box<str>]> {
    let parser = IanaParserExtended::new();
    let mut values = parser
        .iter()
        .map(|record| match record.canonical {
            "Etc/GMT" | "Etc/UTC" => Box::<str>::from("UTC"),
            canonical => Box::<str>::from(canonical),
        })
        .collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    values.into_boxed_slice()
}

/// Calendars required by the current ECMA-402 calendar table, excluding aliases.
const CALENDARS: &[&str] = &[
    "buddhist",
    "chinese",
    "coptic",
    "dangi",
    "ethioaa",
    "ethiopic",
    "gregory",
    "hebrew",
    "indian",
    "islamic-civil",
    "islamic-tbla",
    "islamic-umalqura",
    "iso8601",
    "japanese",
    "persian",
    "roc",
];

/// ICU collations intentionally omit the reserved `search` and `standard` values.
pub(super) const COLLATIONS: &[&str] = &[
    "compat", "dict", "emoji", "eor", "phonebk", "pinyin", "stroke", "trad", "unihan", "zhuyin",
];

/// ISO 4217 codes supported by the pinned ICU capability set.
const CURRENCIES: &[&str] = &[
    "AED", "AFN", "ALL", "AMD", "ANG", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT",
    "BGN", "BHD", "BIF", "BMD", "BND", "BOB", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD",
    "CDF", "CHF", "CLP", "CNY", "COP", "CRC", "CUC", "CUP", "CVE", "CZK", "DJF", "DKK", "DOP",
    "DZD", "EGP", "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP", "GMD", "GNF",
    "GTQ", "GYD", "HKD", "HNL", "HRK", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK",
    "JMD", "JOD", "JPY", "KES", "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK",
    "LBP", "LKR", "LRD", "LSL", "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU",
    "MUR", "MVR", "MWK", "MXN", "MYR", "MZN", "NAD", "NGN", "NIO", "NOK", "NPR", "NZD", "OMR",
    "PAB", "PEN", "PGK", "PHP", "PKR", "PLN", "PYG", "QAR", "RON", "RSD", "RUB", "RWF", "SAR",
    "SBD", "SCR", "SDG", "SEK", "SGD", "SHP", "SLE", "SLL", "SOS", "SRD", "SSP", "STN", "SVC",
    "SYP", "SZL", "THB", "TJS", "TMT", "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH", "UGX",
    "USD", "UYU", "UZS", "VES", "VND", "VUV", "WST", "XAF", "XCD", "XCG", "XDR", "XOF", "XPF",
    "XSU", "YER", "ZAR", "ZMW", "ZWG", "ZWL",
];

/// Every simple-digit numbering system in the current ECMA-402 table.
pub(super) const NUMBERING_SYSTEMS: &[&str] = &[
    "adlm", "ahom", "arab", "arabext", "bali", "beng", "bhks", "brah", "cakm", "cham", "deva",
    "diak", "fullwide", "gara", "gong", "gonm", "gujr", "gukh", "guru", "hanidec", "hmng", "hmnp",
    "java", "kali", "kawi", "khmr", "knda", "krai", "lana", "lanatham", "laoo", "latn", "lepc",
    "limb", "mathbold", "mathdbl", "mathmono", "mathsanb", "mathsans", "mlym", "modi", "mong",
    "mroo", "mtei", "mymr", "mymrepka", "mymrpao", "mymrshan", "mymrtlng", "nagm", "newa", "nkoo",
    "olck", "onao", "orya", "osma", "outlined", "rohg", "saur", "segment", "shrd", "sind", "sinh",
    "sora", "sund", "sunu", "takr", "talu", "tamldec", "telu", "thai", "tibt", "tirh", "tnsa",
    "tols", "vaii", "wara", "wcho",
];

/// Sanctioned simple unit identifiers are a specification table, not locale data.
const UNITS: &[&str] = &[
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_static_collection_is_sorted_unique_and_well_formed() {
        for values in [CALENDARS, COLLATIONS, CURRENCIES, NUMBERING_SYSTEMS, UNITS] {
            assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(
                values
                    .iter()
                    .all(|value| value.is_ascii() && !value.is_empty())
            );
        }
        assert!(CALENDARS.contains(&"gregory"));
        assert!(!COLLATIONS.contains(&"search"));
        assert!(!COLLATIONS.contains(&"standard"));
        assert!(!COLLATIONS.contains(&"phonetic"));
        assert!(!COLLATIONS.contains(&"searchjl"));
        assert!(UNITS.iter().all(|unit| !unit.contains("-per-")));
    }

    #[test]
    fn time_zones_include_every_required_non_continental_primary_identifier() {
        let values = supported_time_zones();
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(values.iter().any(|value| value.as_ref() == "UTC"));
        assert!(values.iter().any(|value| value.as_ref() == "Etc/GMT+12"));
        assert!(values.iter().any(|value| value.as_ref() == "Etc/GMT-14"));
        assert!(!values.iter().any(|value| value.as_ref() == "Etc/GMT"));
        assert!(!values.iter().any(|value| value.as_ref() == "Etc/UTC"));
    }
}
