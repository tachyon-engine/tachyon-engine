use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

struct TestListFormatProvider;

impl IntlProvider for TestListFormatProvider {
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

    fn create_list_format(
        &mut self,
        request: IntlListFormatRequest,
    ) -> Result<IntlListFormatResolved, HostProviderError> {
        Ok(IntlListFormatResolved {
            locale: request
                .locales
                .first()
                .cloned()
                .unwrap_or_else(|| "en-US".into()),
            list_type: request.list_type,
            style: request.style,
        })
    }

    /// Emits deterministic English conjunction parts while preserving every UTF-16 code unit.
    fn format_list(
        &mut self,
        _resolved: &IntlListFormatResolved,
        elements: &[Box<[u16]>],
    ) -> Result<IntlFormattedListParts, HostProviderError> {
        let mut formatted = Vec::new();
        let mut spans = Vec::with_capacity(elements.len().saturating_mul(2));
        for (index, element) in elements.iter().enumerate() {
            if index != 0 {
                let separator: &[u16] = if index + 1 == elements.len() {
                    &[
                        b' ' as u16,
                        b'a' as u16,
                        b'n' as u16,
                        b'd' as u16,
                        b' ' as u16,
                    ]
                } else {
                    &[b',' as u16, b' ' as u16]
                };
                append_list_part(
                    &mut formatted,
                    &mut spans,
                    IntlListFormatPartType::Literal,
                    separator,
                )?;
            }
            append_list_part(
                &mut formatted,
                &mut spans,
                IntlListFormatPartType::Element,
                element,
            )?;
        }
        Ok(IntlFormattedListParts {
            formatted: formatted.into_boxed_slice(),
            spans: spans.into_boxed_slice(),
        })
    }

    fn list_format_supported_locales(
        &mut self,
        locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Ok(locales.into())
    }
}

fn append_list_part(
    formatted: &mut Vec<u16>,
    spans: &mut Vec<IntlListFormatPartSpan>,
    kind: IntlListFormatPartType,
    value: &[u16],
) -> Result<(), HostProviderError> {
    let start = u32::try_from(formatted.len()).map_err(|_| HostProviderError::Failure(6))?;
    formatted.extend_from_slice(value);
    let end = u32::try_from(formatted.len()).map_err(|_| HostProviderError::Failure(6))?;
    spans.push(IntlListFormatPartSpan { kind, start, end });
    Ok(())
}

const LIST_FORMAT_SOURCE: &str = r#"
var trace = "";
var options = {
  get localeMatcher() { trace += "localeMatcher,"; return "best fit"; },
  get type() { trace += "type,"; return "conjunction"; },
  get style() { trace += "style"; return "long"; }
};
var lf = new Intl.ListFormat(["en-US"], options);
var formatted = lf.format(["foo", "bar", "baz"]);
var stringFormatted = lf.format("xy");
var parts = lf.formatToParts(["foo", "bar"]);
var closed = false;
var invalid = {
  [Symbol.iterator]() {
    var done = false;
    return {
      next() { if (done) return { done: true }; done = true; return { value: 1, done: false }; },
      return() { closed = true; return {}; }
    };
  }
};
var invalidType = false;
try { lf.format(invalid); } catch (error) { invalidType = error instanceof TypeError; }
var resolved = lf.resolvedOptions();
var supported = Intl.ListFormat.supportedLocalesOf(["en-US"]);
trace === "localeMatcher,type,style" &&
formatted === "foo, bar and baz" && stringFormatted === "x and y" &&
parts.length === 3 && parts[0].type === "element" && parts[0].value === "foo" &&
parts[1].type === "literal" && parts[1].value === " and " &&
parts[2].type === "element" && parts[2].value === "bar" &&
Object.keys(parts[0]).join(",") === "type,value" && invalidType && closed &&
resolved.locale === "en-US" && resolved.type === "conjunction" && resolved.style === "long" &&
supported.length === 1 && supported[0] === "en-US" &&
Object.getPrototypeOf(lf) === Intl.ListFormat.prototype && Object.isExtensible(lf) &&
Object.prototype.toString.call(lf) === "[object Intl.ListFormat]";
"#;

#[test]
fn list_format_surface_survives_dispatch_batches_and_forced_major() {
    for forced_major in [false, true] {
        assert_list_format_batch::<1>(forced_major);
        assert_list_format_batch::<2>(forced_major);
        assert_list_format_batch::<4>(forced_major);
        assert_list_format_batch::<8>(forced_major);
        assert_list_format_batch::<16>(forced_major);
    }
}

/// Executes constructor, iterable-close, formatting, parts, and MOP surfaces under one policy.
fn assert_list_format_batch<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_980 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("intl-list-format-substrate"),
                MediaType::JavaScript,
                Arc::from(LIST_FORMAT_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("ListFormat substrate fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(41, 43)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new().with_intl(TestListFormatProvider),
    )
    .expect("ListFormat substrate isolate initializes");
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
        .expect("ListFormat substrate executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "ListFormat batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
