use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

struct TestPluralRulesBackend;

impl IntlPluralRulesBackend for TestPluralRulesBackend {
    fn select(
        &self,
        value: &IntlMathematicalValue,
    ) -> Result<IntlPluralCategory, HostProviderError> {
        Ok(match value {
            IntlMathematicalValue::Finite(value) if value.as_ref() == "1" => {
                IntlPluralCategory::One
            }
            _ => IntlPluralCategory::Other,
        })
    }

    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

struct TestPluralRulesProvider;

impl IntlProvider for TestPluralRulesProvider {
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

    fn create_plural_rules(
        &mut self,
        request: IntlPluralRulesRequest,
    ) -> Result<IntlPluralRulesCreation, HostProviderError> {
        Ok(IntlPluralRulesCreation {
            resolved: IntlPluralRulesResolved {
                locale: request
                    .locales
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "en-US".into()),
                rule_type: request.rule_type,
                options: request.options,
                categories: Box::new([IntlPluralCategory::One, IntlPluralCategory::Other]),
            },
            backend: Box::new(TestPluralRulesBackend),
        })
    }

    fn plural_rules_supported_locales(
        &mut self,
        locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Ok(locales.into())
    }
}

const PLURAL_RULES_SOURCE: &str = r#"
var trace = "";
var options = {
  get localeMatcher() { trace += "localeMatcher,"; return "best fit"; },
  get type() { trace += "type,"; return "ordinal"; },
  get notation() { trace += "notation,"; return "standard"; },
  get compactDisplay() { trace += "compactDisplay,"; return "short"; },
  get minimumIntegerDigits() { trace += "minimumIntegerDigits,"; return 1; },
  get minimumFractionDigits() { trace += "minimumFractionDigits,"; return undefined; },
  get maximumFractionDigits() { trace += "maximumFractionDigits,"; return undefined; },
  get minimumSignificantDigits() { trace += "minimumSignificantDigits,"; return undefined; },
  get maximumSignificantDigits() { trace += "maximumSignificantDigits,"; return undefined; },
  get roundingIncrement() { trace += "roundingIncrement,"; return 1; },
  get roundingMode() { trace += "roundingMode,"; return "halfExpand"; },
  get roundingPriority() { trace += "roundingPriority,"; return "auto"; },
  get trailingZeroDisplay() { trace += "trailingZeroDisplay"; return "auto"; }
};
var rules = new Intl.PluralRules(["en-US"], options);
var converted = false;
var selected = rules.select({ valueOf() { converted = true; return 1; } });
var nonFinite = rules.select(Infinity);
var resolved = rules.resolvedOptions();
var supported = Intl.PluralRules.supportedLocalesOf(["en-US"], { localeMatcher: "lookup" });
trace === "localeMatcher,type,notation,compactDisplay,minimumIntegerDigits,minimumFractionDigits,maximumFractionDigits,minimumSignificantDigits,maximumSignificantDigits,roundingIncrement,roundingMode,roundingPriority,trailingZeroDisplay" &&
converted && selected === "one" && nonFinite === "other" &&
resolved.locale === "en-US" && resolved.type === "ordinal" && resolved.notation === "standard" &&
resolved.minimumIntegerDigits === 1 && resolved.minimumFractionDigits === 0 &&
resolved.maximumFractionDigits === 3 && resolved.pluralCategories.join(",") === "one,other" &&
supported.length === 1 && supported[0] === "en-US" &&
Object.getPrototypeOf(rules) === Intl.PluralRules.prototype && Object.isExtensible(rules) &&
Object.prototype.toString.call(rules) === "[object Intl.PluralRules]";
"#;

#[test]
fn plural_rules_surface_survives_dispatch_batches_and_forced_major() {
    for forced_major in [false, true] {
        assert_plural_rules_batch::<1>(forced_major);
        assert_plural_rules_batch::<2>(forced_major);
        assert_plural_rules_batch::<4>(forced_major);
        assert_plural_rules_batch::<8>(forced_major);
        assert_plural_rules_batch::<16>(forced_major);
    }
}

/// Executes option callbacks, ToNumber, selection, resolved slots, and locale filtering.
fn assert_plural_rules_batch<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(11_020 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("intl-plural-rules-substrate"),
                MediaType::JavaScript,
                Arc::from(PLURAL_RULES_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("PluralRules substrate fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(47, 53)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new().with_intl(TestPluralRulesProvider),
    )
    .expect("PluralRules substrate isolate initializes");
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
        .expect("PluralRules substrate executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "PluralRules batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
