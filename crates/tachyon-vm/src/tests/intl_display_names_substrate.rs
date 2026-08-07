use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

struct TestDisplayNamesBackend;

impl IntlDisplayNamesBackend for TestDisplayNamesBackend {
    fn display_name(&self, code: &str) -> Result<Option<Box<[u16]>>, HostProviderError> {
        Ok((code == "US").then(|| "United States".encode_utf16().collect()))
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

struct TestDisplayNamesProvider;

impl IntlProvider for TestDisplayNamesProvider {
    fn canonicalize_locale(&mut self, locale: &str) -> Result<Option<Box<str>>, HostProviderError> {
        Ok(Some(locale.into()))
    }

    fn default_locale(&mut self) -> Result<Box<str>, HostProviderError> {
        Ok("en-US".into())
    }

    fn supported_values(
        &mut self,
        _key: IntlSupportedValuesKey,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Ok(Box::new([]))
    }

    fn create_display_names(
        &mut self,
        request: IntlDisplayNamesRequest,
    ) -> Result<IntlDisplayNamesCreation, HostProviderError> {
        Ok(IntlDisplayNamesCreation {
            resolved: IntlDisplayNamesResolved {
                locale: request
                    .locales
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "en-US".into()),
                style: request.style,
                display_type: request.display_type,
                fallback: request.fallback,
                language_display: request.language_display,
            },
            backend: Box::new(TestDisplayNamesBackend),
        })
    }

    fn display_names_supported_locales(
        &mut self,
        locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Ok(locales.into())
    }
}

const DISPLAY_NAMES_SOURCE: &str = r#"
var trace = "";
var options = {
  get localeMatcher() { trace += "localeMatcher,"; return { toString() { trace += "matcherString,"; return "best fit"; } }; },
  get style() { trace += "style,"; return { toString() { trace += "styleString,"; return "short"; } }; },
  get type() { trace += "type,"; return { toString() { trace += "typeString,"; return "region"; } }; },
  get fallback() { trace += "fallback,"; return { toString() { trace += "fallbackString,"; return "code"; } }; },
  get languageDisplay() { trace += "languageDisplay"; return "standard"; }
};
var formatter = new Intl.DisplayNames(["en-US"], options);
var converted = "";
var localized = formatter.of({ toString() { converted += "code"; return "us"; } });
var fallback = formatter.of("zz");
var resolved = formatter.resolvedOptions();
var supported = Intl.DisplayNames.supportedLocalesOf(["en-US"], { localeMatcher: "lookup" });
trace === "localeMatcher,matcherString,style,styleString,type,typeString,fallback,fallbackString,languageDisplay" &&
converted === "code" && localized === "United States" && fallback === "ZZ" &&
resolved.locale === "en-US" && resolved.style === "short" && resolved.type === "region" &&
resolved.fallback === "code" && resolved.languageDisplay === undefined &&
supported.length === 1 && supported[0] === "en-US" &&
Object.getPrototypeOf(formatter) === Intl.DisplayNames.prototype && Object.isExtensible(formatter) &&
Object.prototype.toString.call(formatter) === "[object Intl.DisplayNames]";
"#;

#[test]
fn display_names_surface_survives_dispatch_batches_and_forced_major() {
    for forced_major in [false, true] {
        assert_display_names_batch::<1>(forced_major);
        assert_display_names_batch::<2>(forced_major);
        assert_display_names_batch::<4>(forced_major);
        assert_display_names_batch::<8>(forced_major);
        assert_display_names_batch::<16>(forced_major);
    }
}

/// Executes prototype lookup, ordered options, code conversion, fallback, and ordinary MOP.
fn assert_display_names_batch<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(11_160 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("intl-display-names-substrate"),
                MediaType::JavaScript,
                Arc::from(DISPLAY_NAMES_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("DisplayNames substrate fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(67, 71)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new().with_intl(TestDisplayNamesProvider),
    )
    .expect("DisplayNames substrate isolate initializes");
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
        .expect("DisplayNames substrate executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "DisplayNames batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
