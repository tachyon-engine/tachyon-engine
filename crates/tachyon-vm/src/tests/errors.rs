use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ERROR_SOURCE: &str = r#"
var trace = "";
var options = new Proxy({
  get cause() { trace += "g"; return 42; }
}, {
  has(target, key) { trace += "h"; return key in target; }
});
var error = new TypeError({
  toString() { trace += "m"; return "boom"; }
}, options);
var text = Error.prototype.toString.call({
  get name() {
    trace += "n";
    return { toString() { trace += "N"; return "Kind"; } };
  },
  get message() {
    trace += "x";
    return { [Symbol.toPrimitive]() { trace += "M"; return "message"; } };
  }
});
trace === "mhgnNxM" &&
error.message === "boom" && error.cause === 42 &&
text === "Kind: message" &&
Object.prototype.toString.call(error) === "[object Error]" &&
Object.getPrototypeOf(TypeError) === Error &&
!Object.getOwnPropertyDescriptor(TypeError, "prototype").writable;
"#;

const ERROR_CONSTRUCTOR_ONLY_SOURCE: &str = r#"
var options = new Proxy({ get cause() { return 42; } }, { has(t, k) { return k in t; } });
var error = new TypeError({ toString() { return "boom"; } }, options);
error.message === "boom" && error.cause === 42;
"#;

const ERROR_TO_STRING_ONLY_SOURCE: &str = r#"
Error.prototype.toString.call({
  get name() { return { toString() { return "Kind"; } }; },
  get message() { return { [Symbol.toPrimitive]() { return "message"; } }; }
}) === "Kind: message";
"#;

const ERROR_OBJECT_MESSAGE_ONLY_SOURCE: &str = r#"
new TypeError({ toString() { return "boom"; } }).message === "boom";
"#;

const ERROR_PRIMITIVE_MESSAGE_ONLY_SOURCE: &str = r#"
new TypeError(42).message === "42";
"#;

const ERROR_CAUSE_ONLY_SOURCE: &str = r#"
var options = new Proxy({ get cause() { return 42; } }, { has(t, k) { return k in t; } });
new TypeError(undefined, options).cause === 42;
"#;

#[test]
fn error_constructor_and_to_string_resume_for_every_dispatch_batch() {
    assert_error_batch::<1>();
    assert_error_batch::<2>();
    assert_error_batch::<4>();
    assert_error_batch::<8>();
    assert_error_batch::<16>();
}

#[test]
fn error_constructor_state_survives_forced_major_collections() {
    let module = compile_error_source(80);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("forced-major Error fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn error_constructor_only_survives_forced_major_collections() {
    assert_forced_major_source(ERROR_CONSTRUCTOR_ONLY_SOURCE, 87);
}

#[test]
fn error_to_string_only_survives_forced_major_collections() {
    assert_forced_major_source(ERROR_TO_STRING_ONLY_SOURCE, 88);
}

#[test]
fn error_message_paths_survive_forced_major_collections() {
    assert_forced_major_source(ERROR_OBJECT_MESSAGE_ONLY_SOURCE, 89);
    assert_forced_major_source(ERROR_PRIMITIVE_MESSAGE_ONLY_SOURCE, 90);
}

#[test]
fn error_cause_only_survives_forced_major_collections() {
    assert_forced_major_source(ERROR_CAUSE_ONLY_SOURCE, 91);
}

/// Executes the nested Error continuation chain with one selected dispatch monomorphization.
fn assert_error_batch<const N: usize>() {
    let module = compile_error_source(80 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("Error fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_error_source(source_id: u32) -> CompiledModule {
    compile_source(ERROR_SOURCE, source_id)
}

/// Runs one focused Error state-machine fixture with every allocation forcing a major collection.
fn assert_forced_major_source(source: &str, source_id: u32) {
    let module = compile_source(source, source_id);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("forced-major Error fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

fn compile_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("error-continuations"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Error fixture compiles")
}
