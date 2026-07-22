use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_CASE_SOURCE: &str = r#"
var trace = "";
var receiver = {
  toString() { trace += "s"; return "Straße"; },
  valueOf() { trace += "v"; return "wrong"; }
};
var symbolError = false;
try {
  String.prototype.toLowerCase.call(Symbol());
} catch (error) {
  symbolError = error instanceof TypeError;
}
String.prototype.toUpperCase.call(receiver) === "STRASSE" &&
trace === "s" &&
"ΟΣ".toLowerCase() === "ος" &&
"\u0130".toLowerCase() === "i\u0307" &&
String.fromCharCode(0xD800).toUpperCase() === String.fromCharCode(0xD800) &&
new String("Mixed").toLowerCase() === "mixed" &&
symbolError;
"#;

#[test]
fn string_case_conversion_is_stable_for_every_dispatch_batch() {
    assert_string_case_batch::<1>();
    assert_string_case_batch::<2>();
    assert_string_case_batch::<4>();
    assert_string_case_batch::<8>();
    assert_string_case_batch::<16>();
}

#[test]
/// Separates receiver and Unicode boundaries so one regression reports its exact failing contract.
fn string_case_conversion_covers_each_receiver_and_unicode_boundary() {
    for (source_id, label, source) in [
        (1_140, "primitive", "'Mixed'.toLowerCase() === 'mixed';"),
        (
            1_148,
            "String prototype",
            "String.prototype.toUpperCase() === '';",
        ),
        (
            1_141,
            "object callback",
            "var o = { toString() { return 'Straße'; } }; String.prototype.toUpperCase.call(o) === 'STRASSE';",
        ),
        (1_142, "final sigma", "'ΟΣ'.toLowerCase() === 'ος';"),
        (
            1_143,
            "unpaired surrogate",
            "String.fromCharCode(0xD800).toUpperCase() === String.fromCharCode(0xD800);",
        ),
        (
            1_144,
            "String wrapper construction",
            "typeof new String('Mixed') === 'object';",
        ),
        (
            1_147,
            "String wrapper toString",
            "new String('Mixed').toString() === 'Mixed';",
        ),
        (
            1_146,
            "String wrapper",
            "new String('Mixed').toLowerCase() === 'mixed';",
        ),
        (
            1_145,
            "Symbol TypeError",
            "var ok = false; try { String.prototype.toLowerCase.call(Symbol()); } catch (e) { ok = e instanceof TypeError; } ok;",
        ),
    ] {
        let outcome = execute_string_case_source::<8>(source, source_id);
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
            "{label} returned {outcome:?}"
        );
    }
}

#[test]
/// Keeps the receiver and callback result rooted while every allocation forces a major collection.
fn string_case_receiver_continuation_survives_forced_major() {
    let source = "var trace = ''; var o = { toString() { trace += 's'; return 'Straße'; } }; String.prototype.toUpperCase.call(o) === 'STRASSE' && trace === 's';";
    let module = compile_string_case_source(source, 1_149);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major String case fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major String case fixture returned {outcome:?}"
    );
}

/// Compiles and executes String case conversion under one dispatch monomorphization.
fn assert_string_case_batch<const N: usize>() {
    let outcome = execute_string_case_source::<N>(STRING_CASE_SOURCE, 1_120 + N as u32);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles and executes one focused String case source under the selected dispatch batch.
fn execute_string_case_source<const N: usize>(source: &str, source_id: u32) -> RunOutcome {
    let module = compile_string_case_source(source, source_id);
    test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("String case fixture executes")
}

/// Compiles one String case fixture without coupling it to an isolate collection policy.
fn compile_string_case_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("string-case-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("String case fixture compiles")
}
