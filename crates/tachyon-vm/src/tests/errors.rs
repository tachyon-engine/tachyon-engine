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

const SUPPRESSED_ERROR_SOURCE: &str = r#"
var trace = "";
var primary = {};
var prior = {};
var error = SuppressedError(primary, prior, {
  toString() { trace += "m"; return "cleanup"; }
});
var keys = Object.getOwnPropertyNames(error);
trace === "m" &&
error instanceof SuppressedError && error instanceof Error &&
error.error === primary && error.suppressed === prior && error.message === "cleanup" &&
keys.indexOf("error") === keys.indexOf("message") + 1 &&
keys.indexOf("suppressed") === keys.indexOf("error") + 1 &&
!Object.prototype.hasOwnProperty.call(new SuppressedError(), "message") &&
Object.getPrototypeOf(SuppressedError) === Error &&
SuppressedError.length === 3;
"#;

const ERROR_STACK_ACCESSOR_SOURCE: &str = r#"
var trace = "";
var descriptor = Object.getOwnPropertyDescriptor(Error.prototype, "stack");
var firstTarget = {};
var first = new Proxy(firstTarget, {
  getOwnPropertyDescriptor(target, key) {
    trace += "g";
    return Object.getOwnPropertyDescriptor(target, key);
  },
  defineProperty(target, key, desc) {
    trace += "d";
    return Reflect.defineProperty(target, key, desc);
  }
});
descriptor.set.call(first, "first");
var secondTarget = { stack: "old" };
var second = new Proxy(secondTarget, {
  getOwnPropertyDescriptor(target, key) {
    trace += "G";
    return Object.getOwnPropertyDescriptor(target, key);
  },
  set(target, key, value) {
    trace += "s";
    target[key] = value;
    return true;
  }
});
descriptor.set.call(second, "second");
var error = new TypeError("message");
trace === "gdGs" &&
firstTarget.stack === "first" && secondTarget.stack === "second" &&
typeof descriptor.get.call(error) === "string" &&
descriptor.get.call({}) === undefined &&
!Object.prototype.hasOwnProperty.call(error, "stack");
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

#[test]
fn suppressed_error_resumes_for_every_dispatch_batch() {
    assert_source_batch::<1>(SUPPRESSED_ERROR_SOURCE, 98, false);
    assert_source_batch::<2>(SUPPRESSED_ERROR_SOURCE, 99, false);
    assert_source_batch::<4>(SUPPRESSED_ERROR_SOURCE, 100, false);
    assert_source_batch::<8>(SUPPRESSED_ERROR_SOURCE, 101, false);
    assert_source_batch::<16>(SUPPRESSED_ERROR_SOURCE, 102, false);
}

#[test]
fn suppressed_error_roots_survive_forced_major_collections() {
    assert_source_batch::<8>(SUPPRESSED_ERROR_SOURCE, 103, true);
}

#[test]
fn error_stack_accessor_resumes_for_every_dispatch_batch() {
    assert_source_batch::<1>(ERROR_STACK_ACCESSOR_SOURCE, 92, false);
    assert_source_batch::<2>(ERROR_STACK_ACCESSOR_SOURCE, 93, false);
    assert_source_batch::<4>(ERROR_STACK_ACCESSOR_SOURCE, 94, false);
    assert_source_batch::<8>(ERROR_STACK_ACCESSOR_SOURCE, 95, false);
    assert_source_batch::<16>(ERROR_STACK_ACCESSOR_SOURCE, 96, false);
}

#[test]
fn error_stack_accessor_roots_survive_forced_major_collections() {
    assert_source_batch::<8>(ERROR_STACK_ACCESSOR_SOURCE, 97, true);
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
    assert_source_batch::<8>(source, source_id, true);
}

/// Runs one Error fixture with a selected dispatch batch and collection policy.
fn assert_source_batch<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_source(source, source_id);
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
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("Error fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
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
