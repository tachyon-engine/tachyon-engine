use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_TRIM_SOURCE: &str = r#"
var trace = "";
var receiver = {
  toString() { trace += "s"; return "  converted  "; },
  valueOf() { trace += "v"; return "wrong"; }
};
var symbolError = false;
try {
  String.prototype.trim.call(Symbol());
} catch (error) {
  symbolError = error instanceof TypeError;
}
var objectSymbolError = false;
try {
  String.prototype.trim.call({ toString() { return Symbol(); } });
} catch (error) {
  objectSymbolError = error instanceof TypeError;
}
var nullError = false;
try {
  String.prototype.trim.call(null);
} catch (error) {
  nullError = error instanceof TypeError;
}
String.prototype.trim.call(false) === "false" &&
String.prototype.trim.call(42) === "42" &&
String.prototype.trim.call(1n) === "1" &&
String.prototype.trimStart.call(true) === "true" &&
String.prototype.trimEnd.call(false) === "false" &&
String.prototype.trim.call(receiver) === "converted" &&
trace === "s" && symbolError && objectSymbolError && nullError;
"#;

#[test]
fn string_trim_conversion_is_stable_for_every_dispatch_batch() {
    assert_string_trim_batch::<1>(false);
    assert_string_trim_batch::<2>(false);
    assert_string_trim_batch::<4>(false);
    assert_string_trim_batch::<8>(false);
    assert_string_trim_batch::<16>(false);
}

#[test]
fn string_trim_conversion_survives_forced_major_collection() {
    assert_string_trim_batch::<8>(true);
}

/// Compiles and executes trim conversion under one dispatch and collection policy.
fn assert_string_trim_batch<const N: usize>(forced_major: bool) {
    let module = compile_string_trim_source(1_160 + N as u32);
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("String trim fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the shared trim fixture without coupling it to a collection policy.
fn compile_string_trim_source(source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("string-trim-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_TRIM_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("String trim fixture compiles")
}
