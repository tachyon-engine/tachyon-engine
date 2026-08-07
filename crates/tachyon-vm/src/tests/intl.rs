use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

struct TestIntlProvider;

impl IntlProvider for TestIntlProvider {
    fn canonicalize_locale(&mut self, locale: &str) -> Result<Option<Box<str>>, HostProviderError> {
        let canonical = match locale {
            "EN-us" | "en-US" => "en-US",
            "en-gb-oxendict" => "en-GB-oxendict",
            "fr-CA" => "fr-CA",
            "jp" => "jp",
            "jp-u-ca-gregory" => "jp-u-ca-gregory",
            "de" => "de",
            "fr" => "fr",
            _ if locale.contains('_') => return Ok(None),
            _ => locale,
        };
        Ok(Some(canonical.into()))
    }

    fn default_locale(&mut self) -> Result<Box<str>, HostProviderError> {
        Ok("en-US".into())
    }

    fn supported_values(
        &mut self,
        key: IntlSupportedValuesKey,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        let values = match key {
            IntlSupportedValuesKey::Calendar => ["gregory", "buddhist", "gregory"].as_slice(),
            IntlSupportedValuesKey::Collation => ["emoji"].as_slice(),
            IntlSupportedValuesKey::Currency => ["USD"].as_slice(),
            IntlSupportedValuesKey::NumberingSystem => ["latn"].as_slice(),
            IntlSupportedValuesKey::TimeZone => ["UTC"].as_slice(),
            IntlSupportedValuesKey::Unit => ["meter"].as_slice(),
        };
        Ok(values
            .iter()
            .map(|value| Box::<str>::from(*value))
            .collect::<Vec<_>>()
            .into_boxed_slice())
    }
}

const INTL_SOURCE: &str = r#"
var trace = "";
var locales = {
  get length() {
    trace += "l";
    return { valueOf() { trace += "n"; return 4; } };
  },
  0: { toString() { trace += "s"; return "EN-us"; } },
  2: "fr",
  3: "en-US"
};
Object.defineProperty(locales, "1", {
  get() { trace += "g"; return "de"; },
  configurable: true
});
Array.prototype.push = function () { throw new Error("push must not be observed"); };
var result = Intl.getCanonicalLocales(locales);
var typeError = false;
var rangeError = false;
try { Intl.getCanonicalLocales([1]); } catch (error) { typeError = error instanceof TypeError; }
try { Intl.getCanonicalLocales(["en_US"]); } catch (error) { rangeError = error instanceof RangeError; }
var tag = Object.getOwnPropertyDescriptor(Intl, Symbol.toStringTag);
trace === "lnsg" && result.length === 3 &&
result[0] === "en-US" && result[1] === "de" && result[2] === "fr" &&
Intl.getCanonicalLocales(undefined).length === 0 &&
Intl.getCanonicalLocales("fr")[0] === "fr" &&
Intl.getCanonicalLocales.name === "getCanonicalLocales" &&
Intl.getCanonicalLocales.length === 1 &&
Object.prototype.toString.call(Intl) === "[object Intl]" &&
tag.value === "Intl" && tag.writable === false &&
tag.enumerable === false && tag.configurable === true && typeError && rangeError;
"#;

const SUPPORTED_VALUES_SOURCE: &str = r#"
var trace = "";
var key = {
  get toString() {
    trace += "g";
    return function () { trace += "c"; return "calendar"; };
  }
};
Array.prototype.push = function () { throw new Error("push must not be observed"); };
Array.prototype.sort = function () { throw new Error("sort must not be observed"); };
var first = Intl.supportedValuesOf(key);
var second = Intl.supportedValuesOf("calendar");
var invalidRange = false;
var symbolType = false;
var constructType = false;
try { Intl.supportedValuesOf("calendar\0"); } catch (error) { invalidRange = error instanceof RangeError; }
try { Intl.supportedValuesOf(Symbol()); } catch (error) { symbolType = error instanceof TypeError; }
try { new Intl.supportedValuesOf("calendar"); } catch (error) { constructType = error instanceof TypeError; }
var descriptor = Object.getOwnPropertyDescriptor(Intl, "supportedValuesOf");
trace === "gc" && Array.isArray(first) && first !== second &&
Object.getPrototypeOf(first) === Array.prototype && first.length === 2 &&
first[0] === "buddhist" && first[1] === "gregory" &&
Intl.supportedValuesOf("collation")[0] === "emoji" &&
Intl.supportedValuesOf("currency")[0] === "USD" &&
Intl.supportedValuesOf("numberingSystem")[0] === "latn" &&
Intl.supportedValuesOf("timeZone")[0] === "UTC" &&
Intl.supportedValuesOf("unit")[0] === "meter" &&
Intl.supportedValuesOf.name === "supportedValuesOf" &&
Intl.supportedValuesOf.length === 1 &&
!("prototype" in Intl.supportedValuesOf) && Object.isExtensible(Intl.supportedValuesOf) &&
descriptor.writable && !descriptor.enumerable && descriptor.configurable &&
invalidRange && symbolType && constructType;
"#;

#[test]
fn intl_locale_lists_are_stable_for_every_dispatch_batch() {
    assert_intl_batch::<1>(false);
    assert_intl_batch::<2>(false);
    assert_intl_batch::<4>(false);
    assert_intl_batch::<8>(false);
    assert_intl_batch::<16>(false);
}

#[test]
fn intl_locale_lists_survive_forced_major_collection() {
    assert_intl_batch::<8>(true);
}

#[test]
fn intl_supported_values_are_stable_for_every_dispatch_batch() {
    assert_supported_values_batch::<1>(false);
    assert_supported_values_batch::<2>(false);
    assert_supported_values_batch::<4>(false);
    assert_supported_values_batch::<8>(false);
    assert_supported_values_batch::<16>(false);
}

#[test]
fn intl_supported_values_survive_forced_major_collection() {
    assert_supported_values_batch::<8>(true);
}

#[test]
fn intl_provider_absence_remains_an_explicit_host_error() {
    let module = compile_intl_source("Intl.getCanonicalLocales('en');", 10_099);
    let error = test_isolate_without_intl()
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect_err("Intl without a provider must not consult process locale");
    assert_eq!(error, ExecutionError::MissingIntlProvider);
}

#[test]
fn intl_supported_values_provider_absence_is_explicit_after_key_validation() {
    let module = compile_intl_source("Intl.supportedValuesOf('calendar');", 10_098);
    let error = test_isolate_without_intl()
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect_err("Intl enumeration without a provider must stay explicit");
    assert_eq!(error, ExecutionError::MissingIntlProvider);
}

#[test]
fn intl_locale_objects_feed_canonical_locale_lists() {
    let cases = [
        (
            "new Intl.Locale('en-gb-oxendict') instanceof Intl.Locale",
            "basic Locale construction",
        ),
        (
            "new Intl.Locale('en-gb-oxendict').toString()",
            "Locale toString",
        ),
        (
            "new Intl.Locale('jp', { calendar: 'gregory' }).toString() === 'jp-u-ca-gregory'",
            "Locale calendar option",
        ),
        (
            "Intl.getCanonicalLocales([new Intl.Locale('fr-CA')])[0] === 'fr-CA'",
            "Locale list internal tag",
        ),
        (
            "new Intl.Locale('und', { calendar: 'islamic-civil' }).calendar === 'islamic-civil'",
            "Locale calendar getter",
        ),
        (
            "new Intl.Locale('und', { collation: 'emoji' }).collation === 'emoji'",
            "Locale collation getter",
        ),
        (
            "new Intl.Locale('und', { numberingSystem: 'latn' }).numberingSystem === 'latn'",
            "Locale numbering-system getter",
        ),
        (
            "new Intl.Locale(new Intl.Locale('fr-CA')).toString() === 'fr-CA'",
            "Locale constructor copies the internal tag",
        ),
        (
            "Object.prototype.toString.call(new Intl.Locale('en')) === '[object Intl.Locale]'",
            "Locale toStringTag",
        ),
        (
            "(() => { try { Intl.Locale.prototype.toString.call(new String('en')); return false; } catch (error) { return error instanceof TypeError; } })()",
            "Locale toString rejects String wrappers",
        ),
        (
            "(() => { let get = Object.getOwnPropertyDescriptor(Intl.Locale.prototype, 'calendar').get; try { get.call(new String('en')); return false; } catch (error) { return error instanceof TypeError; } })()",
            "Locale accessors reject String wrappers",
        ),
        (
            "(() => { let locale = new Intl.Locale('de-Latn-DE-1996-fonipa-u-hc-h23-kf-kn-false'); return locale.baseName === 'de-Latn-DE-1996-fonipa' && locale.language === 'de' && locale.script === 'Latn' && locale.region === 'DE' && locale.variants === '1996-fonipa' && locale.hourCycle === 'h23' && locale.caseFirst === '' && locale.numeric === false; })()",
            "Locale base and Unicode-key getters",
        ),
        (
            "(() => { let locale = new Intl.Locale('und'); return locale.language === 'und' && locale.script === undefined && locale.region === undefined && locale.variants === undefined && locale.numeric === false; })()",
            "Locale missing components",
        ),
    ];
    for (index, (source, label)) in cases.into_iter().enumerate() {
        let module = compile_intl_source(source, 10_100 + index as u32);
        let mut isolate = intl_test_isolate();
        let outcome = isolate
            .execute_with_batch::<8>(
                &module,
                ExecutionBudget {
                    fuel: 8_192,
                    quantum: 8_192,
                },
            )
            .unwrap_or_else(|error| panic!("{label} executes: {error:?}"));
        if index == 1 {
            let RunOutcome::Completed(value) = outcome else {
                panic!("{label} returned {outcome:?}");
            };
            assert_eq!(
                String::from_utf16_lossy(&isolate.string_value_to_utf16(value).unwrap()),
                "en-GB-oxendict"
            );
            continue;
        }
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
            "{label} returned {outcome:?}"
        );
    }
}

/// Executes the observable locale-list fixture under one dispatch and collection policy.
fn assert_intl_batch<const N: usize>(forced_major: bool) {
    let module = compile_intl_source(INTL_SOURCE, 10_000 + N as u32);
    let mut isolate = intl_test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("Intl locale-list fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Intl batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes provider enumeration under one dispatch and moving-collection policy.
fn assert_supported_values_batch<const N: usize>(forced_major: bool) {
    let module = compile_intl_source(SUPPORTED_VALUES_SOURCE, 10_200 + N as u32);
    let mut isolate = intl_test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("Intl supported-values fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Intl supported-values batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

fn intl_test_isolate() -> Isolate {
    Isolate::new_with_host_providers(
        test_isolate_config(),
        HostProviders::new().with_intl(TestIntlProvider),
    )
    .expect("Intl provider test isolate descriptors register")
}

fn test_isolate_without_intl() -> Isolate {
    Isolate::new(test_isolate_config()).expect("Intl host-error test isolate descriptors register")
}

fn test_isolate_config() -> IsolateConfig {
    IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(9 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    )
}

fn compile_intl_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("intl-locale-list"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Intl locale-list fixture compiles")
}
