use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

struct TestRelativeTimeFormatBackend;

impl IntlRelativeTimeFormatBackend for TestRelativeTimeFormatBackend {
    fn format(
        &self,
        _value: &IntlMathematicalValue,
        _unit: IntlRelativeTimeUnit,
    ) -> Result<Box<[u16]>, HostProviderError> {
        Ok("in 1 day".encode_utf16().collect())
    }

    fn format_to_parts(
        &self,
        _value: &IntlMathematicalValue,
        _unit: IntlRelativeTimeUnit,
    ) -> Result<IntlFormattedRelativeTimeParts, HostProviderError> {
        Ok(IntlFormattedRelativeTimeParts {
            formatted: "in 1 day".encode_utf16().collect(),
            spans: Box::new([
                IntlRelativeTimePartSpan {
                    kind: IntlNumberFormatPartType::Literal,
                    start: 0,
                    end: 3,
                    has_unit: false,
                },
                IntlRelativeTimePartSpan {
                    kind: IntlNumberFormatPartType::Integer,
                    start: 3,
                    end: 4,
                    has_unit: true,
                },
                IntlRelativeTimePartSpan {
                    kind: IntlNumberFormatPartType::Literal,
                    start: 4,
                    end: 8,
                    has_unit: false,
                },
            ]),
        })
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

struct TestRelativeTimeFormatProvider;

impl IntlProvider for TestRelativeTimeFormatProvider {
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

    fn create_relative_time_format(
        &mut self,
        request: IntlRelativeTimeFormatRequest,
    ) -> Result<IntlRelativeTimeFormatCreation, HostProviderError> {
        Ok(IntlRelativeTimeFormatCreation {
            resolved: IntlRelativeTimeFormatResolved {
                locale: request
                    .locales
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "en-US".into()),
                numbering_system: request.numbering_system.unwrap_or_else(|| "latn".into()),
                style: request.style,
                numeric: request.numeric,
            },
            backend: Box::new(TestRelativeTimeFormatBackend),
        })
    }

    fn relative_time_format_supported_locales(
        &mut self,
        locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Ok(locales.into())
    }
}

const RELATIVE_TIME_FORMAT_SOURCE: &str = r#"
var trace = "";
var options = {
  get localeMatcher() { trace += "localeMatcher,"; return { toString() { trace += "matcherString,"; return "best fit"; } }; },
  get numberingSystem() { trace += "numberingSystem,"; return { toString() { trace += "numberingString,"; return "latn"; } }; },
  get style() { trace += "style,"; return { toString() { trace += "styleString,"; return "short"; } }; },
  get numeric() { trace += "numeric,"; return { toString() { trace += "numericString"; return "auto"; } }; }
};
var formatter = new Intl.RelativeTimeFormat(["en-US"], options);
var converted = "";
var formatted = formatter.format(
  { valueOf() { converted += "value,"; return 1; } },
  { toString() { converted += "unit"; return "days"; } }
);
var parts = formatter.formatToParts(1, "day");
var resolved = formatter.resolvedOptions();
var supported = Intl.RelativeTimeFormat.supportedLocalesOf(["en-US"], { localeMatcher: "lookup" });
trace === "localeMatcher,matcherString,numberingSystem,numberingString,style,styleString,numeric,numericString" &&
converted === "value,unit" && formatted === "in 1 day" &&
parts.length === 3 && parts[0].type === "literal" && parts[0].value === "in " &&
parts[1].type === "integer" && parts[1].value === "1" && parts[1].unit === "day" &&
parts[2].type === "literal" && parts[2].value === " day" && parts[2].unit === undefined &&
resolved.locale === "en-US" && resolved.style === "short" && resolved.numeric === "auto" &&
resolved.numberingSystem === "latn" && supported.length === 1 && supported[0] === "en-US" &&
Object.getPrototypeOf(formatter) === Intl.RelativeTimeFormat.prototype && Object.isExtensible(formatter) &&
Object.prototype.toString.call(formatter) === "[object Intl.RelativeTimeFormat]";
"#;

#[test]
fn relative_time_format_surface_survives_dispatch_batches_and_forced_major() {
    for forced_major in [false, true] {
        assert_relative_time_format_batch::<1>(forced_major);
        assert_relative_time_format_batch::<2>(forced_major);
        assert_relative_time_format_batch::<4>(forced_major);
        assert_relative_time_format_batch::<8>(forced_major);
        assert_relative_time_format_batch::<16>(forced_major);
    }
}

/// Executes options, argument conversions, provider calls, parts, and resolved scalar slots.
fn assert_relative_time_format_batch<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(11_080 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("intl-relative-time-format-substrate"),
                MediaType::JavaScript,
                Arc::from(RELATIVE_TIME_FORMAT_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("RelativeTimeFormat substrate fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(59, 61)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new().with_intl(TestRelativeTimeFormatProvider),
    )
    .expect("RelativeTimeFormat substrate isolate initializes");
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
        .expect("RelativeTimeFormat substrate executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "RelativeTimeFormat batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
