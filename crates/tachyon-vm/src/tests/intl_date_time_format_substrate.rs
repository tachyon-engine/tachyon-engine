use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

const DATE_TIME_FORMAT_SOURCE: &str = r#"
const formatter = new Intl.DateTimeFormat("en-US", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit"
});
const resolved = formatter.resolvedOptions();
const format = formatter.format;
const parts = formatter.formatToParts(new Date(0));
const range = formatter.formatRange({ valueOf() { return 0; } }, new Date(1));
const rangeParts = formatter.formatRangeToParts(new Date(0), { valueOf() { return 1; } });
let singleValueOf = 0;
const singleParts = formatter.formatToParts({
  valueOf() { singleValueOf += 1; return 0; }
});
const marker = {};
let abruptIdentity = false;
try {
  formatter.formatToParts({ valueOf() { throw marker; } });
} catch (error) {
  abruptIdentity = error === marker;
}
let optionGets = 0;
const observed = new Intl.DateTimeFormat("en-US", new Proxy({
  year: { toString() { return "numeric"; } },
  fractionalSecondDigits: { valueOf() { return 2; } }
}, {
  get(target, key) {
    optionGets += 1;
    return target[key];
  }
}));
const observedResolved = observed.resolvedOptions();
const supported = Intl.DateTimeFormat.supportedLocalesOf(["en-US", "zxx"]);
const legacyReceiver = Object.create(Intl.DateTimeFormat.prototype);
const legacy = Intl.DateTimeFormat.call(legacyReceiver, "en-US", { year: "numeric" });
const legacySymbol = Object.getOwnPropertySymbols(legacy)[0];
const legacyDescriptor = Object.getOwnPropertyDescriptor(legacy, legacySymbol);
const legacyResolved = legacy.resolvedOptions();
const legacyFormat = legacy.format;
let seenLegacySymbol;
const legacyProxy = new Proxy(legacy, {
  get(target, key) {
    seenLegacySymbol = key;
    return target[key];
  }
});
const proxyResolved = Intl.DateTimeFormat.prototype.resolvedOptions.call(legacyProxy);
let emptyTimeZoneRejected = false;
try {
  new Intl.DateTimeFormat("en-US", { timeZone: "" });
} catch (error) {
  emptyTimeZoneRejected = error instanceof RangeError;
}
resolved.locale === "en-US" && resolved.calendar === "gregory" &&
resolved.numberingSystem === "latn" && resolved.timeZone === "UTC" &&
resolved.year === "numeric" && resolved.month === "2-digit" &&
resolved.day === "2-digit" && formatter.format === format &&
format(0) === "01/01/1970" && parts.length === 5 &&
parts[0].type === "month" && parts[0].value === "01" &&
parts[4].type === "year" && parts[4].value === "1970" &&
range === "01/01/1970" && rangeParts.length === 5 &&
rangeParts[0].type === "month" && rangeParts[0].source === "shared" &&
rangeParts[4].value === "1970" && rangeParts[4].source === "shared" &&
singleParts.length === 5 && singleValueOf === 1 && abruptIdentity &&
optionGets === 20 && observedResolved.year === "numeric" &&
observedResolved.fractionalSecondDigits === 2 &&
supported.length === 1 && supported[0] === "en-US" && legacy === legacyReceiver &&
legacySymbol.description === "IntlLegacyConstructedSymbol" &&
legacyDescriptor.writable === false && legacyDescriptor.enumerable === false &&
legacyDescriptor.configurable === false && legacyResolved.year === "numeric" &&
legacyFormat(0) === "01/01/1970" && proxyResolved.year === "numeric" &&
seenLegacySymbol === legacySymbol && emptyTimeZoneRejected
"#;

struct TestDateTimeBackend;

impl IntlDateTimeFormatBackend for TestDateTimeBackend {
    fn format(&self, _input: IntlDateTimeInput) -> Result<Box<[u16]>, HostProviderError> {
        Ok("01/01/1970"
            .encode_utf16()
            .collect::<Vec<_>>()
            .into_boxed_slice())
    }

    fn format_to_parts(
        &self,
        input: IntlDateTimeInput,
    ) -> Result<IntlFormattedDateTimeParts, HostProviderError> {
        let formatted = self.format(input)?;
        Ok(IntlFormattedDateTimeParts {
            formatted,
            spans: vec![
                IntlDateTimePartSpan {
                    kind: IntlDateTimePartType::Month,
                    start: 0,
                    end: 2,
                },
                IntlDateTimePartSpan {
                    kind: IntlDateTimePartType::Literal,
                    start: 2,
                    end: 3,
                },
                IntlDateTimePartSpan {
                    kind: IntlDateTimePartType::Day,
                    start: 3,
                    end: 5,
                },
                IntlDateTimePartSpan {
                    kind: IntlDateTimePartType::Literal,
                    start: 5,
                    end: 6,
                },
                IntlDateTimePartSpan {
                    kind: IntlDateTimePartType::Year,
                    start: 6,
                    end: 10,
                },
            ]
            .into_boxed_slice(),
        })
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

struct TestDateTimeProvider;

impl IntlProvider for TestDateTimeProvider {
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

    fn create_date_time_format(
        &mut self,
        request: IntlDateTimeFormatRequest,
    ) -> Result<IntlDateTimeFormatCreation, HostProviderError> {
        Ok(IntlDateTimeFormatCreation {
            resolved: IntlDateTimeFormatResolved {
                locale: request
                    .locales
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "en-US".into()),
                calendar: request.calendar.unwrap_or_else(|| "gregory".into()),
                numbering_system: request.numbering_system.unwrap_or_else(|| "latn".into()),
                time_zone: request
                    .time_zone
                    .expect("VM canonicalizes a DateTimeFormat time zone before provider creation"),
                hour_cycle: None,
                options: request.options,
            },
            backend: Box::new(TestDateTimeBackend),
        })
    }

    fn date_time_format_supported_locales(
        &mut self,
        locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Ok(locales
            .iter()
            .filter(|locale| locale.as_ref() != "zxx")
            .cloned()
            .collect::<Vec<_>>()
            .into_boxed_slice())
    }

    fn canonicalize_time_zone(
        &mut self,
        identifier: &str,
    ) -> Result<Option<Box<str>>, HostProviderError> {
        Ok(identifier.eq_ignore_ascii_case("UTC").then(|| "UTC".into()))
    }
}

struct TestUtcTimeZone;

impl TimeZoneProvider for TestUtcTimeZone {
    fn default_time_zone_identifier(&mut self) -> Result<Box<str>, HostProviderError> {
        Ok("UTC".into())
    }

    fn offset_milliseconds_for_utc(
        &mut self,
        _utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Ok(0)
    }

    fn utc_milliseconds_for_local(
        &mut self,
        local_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Ok(local_milliseconds)
    }
}

struct ExplicitOnlyTimeZone;

impl TimeZoneProvider for ExplicitOnlyTimeZone {
    fn default_time_zone_identifier(&mut self) -> Result<Box<str>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    fn offset_milliseconds_for_utc(
        &mut self,
        _utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    fn utc_milliseconds_for_local(
        &mut self,
        _local_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }
}

struct TestWallClock;

impl WallClockProvider for TestWallClock {
    fn unix_time_milliseconds(&mut self) -> Result<i64, HostProviderError> {
        Ok(0)
    }
}

#[test]
fn date_time_format_surface_survives_dispatch_batches_and_forced_major() {
    for forced_major in [false, true] {
        assert_date_time_format_batch::<1>(forced_major);
        assert_date_time_format_batch::<2>(forced_major);
        assert_date_time_format_batch::<4>(forced_major);
        assert_date_time_format_batch::<8>(forced_major);
        assert_date_time_format_batch::<16>(forced_major);
    }
}

#[test]
/// Proves an explicit time zone does not consult an unavailable default-zone capability.
fn explicit_time_zone_skips_default_provider_lookup() {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_900),
                SourceName::new("intl-date-time-format-explicit-time-zone"),
                MediaType::JavaScript,
                Arc::from(
                    "new Intl.DateTimeFormat('en-US', { timeZone: 'UTC' }).resolvedOptions().timeZone === 'UTC'",
                ),
            ),
            CompileOptions::default(),
        )
        .expect("explicit DateTimeFormat time-zone fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        date_time_test_config(),
        HostProviders::new()
            .with_wall_clock(TestWallClock)
            .with_time_zone(ExplicitOnlyTimeZone)
            .with_intl(TestDateTimeProvider),
    )
    .expect("explicit DateTimeFormat time-zone isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("explicit DateTimeFormat time zone executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "explicit DateTimeFormat time zone returned {outcome:?}"
    );
}

/// Executes the provider-backed surface under one dispatch and collection policy.
fn assert_date_time_format_batch<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_800 + N as u32),
                SourceName::new("intl-date-time-format-substrate"),
                MediaType::JavaScript,
                Arc::from(DATE_TIME_FORMAT_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("DateTimeFormat substrate fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        date_time_test_config(),
        HostProviders::new()
            .with_wall_clock(TestWallClock)
            .with_time_zone(TestUtcTimeZone)
            .with_intl(TestDateTimeProvider),
    )
    .expect("DateTimeFormat substrate isolate initializes");
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
        .expect("DateTimeFormat substrate executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "DateTimeFormat batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

fn date_time_test_config() -> IsolateConfig {
    IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(9 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    )
}
