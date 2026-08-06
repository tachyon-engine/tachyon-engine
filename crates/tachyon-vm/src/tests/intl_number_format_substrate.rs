use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

struct TestNumberFormatBackend {
    backing: Box<[u8]>,
}

impl IntlNumberFormatBackend for TestNumberFormatBackend {
    fn format(&self, value: &IntlMathematicalValue) -> Result<Box<[u16]>, HostProviderError> {
        let value = match value {
            IntlMathematicalValue::Finite(value) => value.as_ref(),
            IntlMathematicalValue::NegativeZero => "-0",
            IntlMathematicalValue::PositiveInfinity => "Infinity",
            IntlMathematicalValue::NegativeInfinity => "-Infinity",
            IntlMathematicalValue::NaN => "NaN",
        };
        Ok(value.encode_utf16().collect::<Vec<_>>().into_boxed_slice())
    }

    fn format_to_parts(
        &self,
        value: &IntlMathematicalValue,
    ) -> Result<IntlFormattedNumberParts, HostProviderError> {
        let formatted = self.format(value)?;
        let end = u32::try_from(formatted.len()).map_err(|_| HostProviderError::Failure(1))?;
        let spans = match value {
            IntlMathematicalValue::NaN => vec![IntlNumberFormatPartSpan {
                kind: IntlNumberFormatPartType::Nan,
                start: 0,
                end,
            }],
            IntlMathematicalValue::PositiveInfinity | IntlMathematicalValue::NegativeInfinity => {
                vec![IntlNumberFormatPartSpan {
                    kind: IntlNumberFormatPartType::Infinity,
                    start: 0,
                    end,
                }]
            }
            _ => test_decimal_spans(&formatted)?,
        };
        Ok(IntlFormattedNumberParts {
            formatted,
            spans: spans.into_boxed_slice(),
        })
    }

    fn external_memory_bytes(&self) -> usize {
        self.backing.len()
    }
}

struct TestNumberFormatProvider;

impl IntlProvider for TestNumberFormatProvider {
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

    fn create_number_format(
        &mut self,
        request: IntlNumberFormatRequest,
    ) -> Result<IntlNumberFormatCreation, HostProviderError> {
        Ok(IntlNumberFormatCreation {
            resolved: IntlNumberFormatResolved {
                locale: "en-US".into(),
                numbering_system: "latn".into(),
                options: request.options,
            },
            backend: Box::new(TestNumberFormatBackend {
                backing: Box::new([]),
            }),
        })
    }

    fn number_format_supported_locales(
        &mut self,
        locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Ok(locales.into())
    }
}

/// Splits the deterministic test backend's ASCII decimal spelling into typed spans.
fn test_decimal_spans(
    formatted: &[u16],
) -> Result<Vec<IntlNumberFormatPartSpan>, HostProviderError> {
    let mut spans = Vec::with_capacity(4);
    let mut cursor = 0_usize;
    if formatted.first() == Some(&u16::from(b'-')) {
        spans.push(IntlNumberFormatPartSpan {
            kind: IntlNumberFormatPartType::MinusSign,
            start: 0,
            end: 1,
        });
        cursor = 1;
    }
    let decimal = formatted.iter().position(|unit| *unit == u16::from(b'.'));
    let integer_end = decimal.unwrap_or(formatted.len());
    spans.push(IntlNumberFormatPartSpan {
        kind: IntlNumberFormatPartType::Integer,
        start: u32::try_from(cursor).map_err(|_| HostProviderError::Failure(1))?,
        end: u32::try_from(integer_end).map_err(|_| HostProviderError::Failure(1))?,
    });
    if let Some(decimal) = decimal {
        let fraction = decimal
            .checked_add(1)
            .ok_or(HostProviderError::Failure(1))?;
        spans.push(IntlNumberFormatPartSpan {
            kind: IntlNumberFormatPartType::Decimal,
            start: u32::try_from(decimal).map_err(|_| HostProviderError::Failure(1))?,
            end: u32::try_from(fraction).map_err(|_| HostProviderError::Failure(1))?,
        });
        spans.push(IntlNumberFormatPartSpan {
            kind: IntlNumberFormatPartType::Fraction,
            start: u32::try_from(fraction).map_err(|_| HostProviderError::Failure(1))?,
            end: u32::try_from(formatted.len()).map_err(|_| HostProviderError::Failure(1))?,
        });
    }
    Ok(spans)
}

#[test]
/// Proves payload, cache, prototype, shape, and external accounting survive forced major GC.
fn number_format_payload_and_properties_survive_forced_major_collections() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let prototype = isolate.realm.object_prototype.unwrap();
    let number_format = isolate
        .allocate_intl_number_format_object(
            IntlNumberFormatCreation {
                resolved: IntlNumberFormatResolved {
                    locale: "en-US".into(),
                    numbering_system: "latn".into(),
                    options: IntlNumberFormatOptions::default(),
                },
                backend: Box::new(TestNumberFormatBackend {
                    backing: vec![0; 48].into_boxed_slice(),
                }),
            },
            prototype,
            AllocationSpace::Young,
        )
        .unwrap();
    isolate.fiber.registers.push(number_format);

    let property = isolate.intern_intrinsic_name(b"property").unwrap();
    isolate
        .set_own_data_property(number_format, property, Value::from_i32(23))
        .unwrap();
    let replacement_prototype = isolate.create_ordinary_object().unwrap();
    let number_format = isolate.fiber.registers[0];
    assert!(
        isolate
            .ordinary_set_prototype_of(number_format, replacement_prototype)
            .unwrap()
    );
    let (receiver, _) = isolate.object_snapshot(number_format).unwrap();
    isolate.set_object_extensible(receiver, false).unwrap();

    let raw = number_format.as_heap_ref().unwrap();
    let object_ref = isolate
        .heap
        .checked_reference(raw, isolate.types.intl_number_format_object)
        .unwrap();
    isolate.heap.with_running_scope(|scope| {
        let object_ref = scope.root(object_ref).unwrap();
        let object = scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(object_ref, isolate.types.intl_number_format_object)
                .copied()
                .unwrap()
        });
        assert_eq!(object.ordinary.prototype, replacement_prototype);
        assert!(!object.ordinary.extensible);
        assert_eq!(
            object.cached_bound_format.as_immediate(),
            Some(Immediate::Undefined)
        );
        let payload = scope.root(object.payload).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            let payload = no_gc
                .borrow(payload, isolate.types.intl_number_format_payload)
                .unwrap();
            assert_eq!(&*payload.resolved.locale, "en-US");
            assert_eq!(&*payload.resolved.numbering_system, "latn");
            assert!(payload.external_memory_bytes() >= 48);
            assert_eq!(
                payload
                    .backend
                    .format(&IntlMathematicalValue::Finite("123".into())),
                Ok("123".encode_utf16().collect::<Vec<_>>().into_boxed_slice())
            );
        });
    });
    assert_eq!(
        isolate.get_data_property(number_format, property).unwrap(),
        Some(Value::from_i32(23))
    );
}

#[test]
fn number_format_parts_surface_and_records_survive_forced_major_collections() {
    let source = r#"
var nf = new Intl.NumberFormat();
var descriptor = Object.getOwnPropertyDescriptor(Intl.NumberFormat.prototype, "formatToParts");
var trace = "";
var argument = { [Symbol.toPrimitive](hint) { trace += hint; return 12.5; } };
Array.prototype.push = function () { throw new Error("push must not be observed"); };
var parts = nf.formatToParts(argument);
var invalid = false;
try { nf.formatToParts.call({}); } catch (error) { invalid = error instanceof TypeError; }
trace === "number" && Array.isArray(parts) && parts.length === 3 &&
parts[0].type === "integer" && parts[0].value === "12" &&
parts[1].type === "decimal" && parts[1].value === "." &&
parts[2].type === "fraction" && parts[2].value === "5" &&
Object.keys(parts[0]).join(",") === "type,value" &&
nf.formatToParts.name === "formatToParts" && nf.formatToParts.length === 1 &&
descriptor.writable && !descriptor.enumerable && descriptor.configurable && invalid;
"#;
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_950),
                SourceName::new("intl-number-format-parts"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("NumberFormat parts fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(31, 37)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new().with_intl(TestNumberFormatProvider),
    )
    .expect("NumberFormat parts fixture isolate initializes");
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("NumberFormat parts fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "NumberFormat parts fixture returned {outcome:?}"
    );
}

#[test]
/// Roots unpublished toLocaleString formatters and legacy fallback edges under forced major GC.
fn number_format_legacy_and_to_locale_string_survive_forced_major_collections() {
    let source = r#"
var wrapper = Object.create(Intl.NumberFormat.prototype);
var returned = Intl.NumberFormat.call(wrapper, "en-US");
var symbols = Object.getOwnPropertySymbols(wrapper);
var fallback = symbols[0];
var seen = null;
var proxy = new Proxy(wrapper, {
  get(target, key) {
    seen = key;
    return target[key];
  }
});
var options = Intl.NumberFormat.prototype.resolvedOptions.call(proxy);
var format = Object.getOwnPropertyDescriptor(Intl.NumberFormat.prototype, "format").get.call(proxy);
var output = "";
for (var index = 0; index < 32; index++) {
  output = (123).toLocaleString(undefined, { style: "unit", unit: "meter" });
}
returned === wrapper && symbols.length === 1 &&
fallback.description === "IntlLegacyConstructedSymbol" && seen === fallback &&
options.locale === "en-US" && format(7) === "7" && output === "123";
"#;
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_951),
                SourceName::new("intl-number-format-legacy-gc"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("NumberFormat legacy GC fixture compiles");
    let mut isolate = Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(41, 43)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new().with_intl(TestNumberFormatProvider),
    )
    .expect("NumberFormat legacy GC fixture isolate initializes");
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 524_288,
                quantum: 524_288,
            },
        )
        .expect("NumberFormat legacy GC fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "NumberFormat legacy GC fixture returned {outcome:?}"
    );
}
