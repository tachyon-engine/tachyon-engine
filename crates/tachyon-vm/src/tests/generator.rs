use super::fixtures::*;
use super::*;
use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

const GENERATOR_SOURCE: &str = r#"
var entered = 0;
function* values(value) {
    entered = entered + 1;
    return value + 1;
}
var functionPrototype = Object.getPrototypeOf(values);
var ownPrototype = values.prototype;
var generatorPrototype = Object.getPrototypeOf(ownPrototype);
var nextMetadata = generatorPrototype.next.name === "next" && generatorPrototype.next.length === 1;
var generator = values(41);
var delayed = entered === 0;
var instanceChain = Object.getPrototypeOf(generator) === ownPrototype;
var functionChain = functionPrototype !== Function.prototype &&
    Object.getPrototypeOf(functionPrototype) === Function.prototype;
var descriptor = Object.getOwnPropertyDescriptor(values, "prototype");
var descriptorOk = descriptor.value === ownPrototype && !descriptor.writable &&
    !descriptor.enumerable && descriptor.configurable;
var first = generator.next();
var second = generator.next();
var third = generator.next();
var completionOk = entered === 1 && first.value === 42 && first.done &&
    second.value === undefined && second.done && third.value === undefined && third.done;

var brandRejected = false;
try { generatorPrototype.next.call({}); }
catch (error) { brandRejected = error instanceof TypeError; }
var newRejected = false;
try { new values(); }
catch (error) { newRejected = error instanceof TypeError; }

var marker = {};
function* fail() { throw marker; }
var failed = fail();
var throwIdentity = false;
try { failed.next(); }
catch (error) { throwIdentity = error === marker; }
var afterThrow = failed.next();

var reentrant;
function* reenter() { return reentrant.next(); }
reentrant = reenter();
var executingRejected = false;
try { reentrant.next(); }
catch (error) { executingRejected = error instanceof TypeError; }
var afterReentry = reentrant.next();

delayed && instanceChain && functionChain && nextMetadata && descriptorOk && completionOk &&
    brandRejected && newRejected && throwIdentity && afterThrow.done &&
    afterThrow.value === undefined && executingRejected && afterReentry.done;
"#;

const GENERATOR_YIELD_SOURCE: &str = r#"
var entered = 0;
function* exchange() {
    entered = entered + 1;
    var first = yield 1;
    var second = yield first + 1;
    return second + 1;
}
var generator = exchange();
var before = entered === 0;
var first = generator.next(999);
var second = generator.next(40);
var third = generator.next(41);
var fourth = generator.next(100);
var exchangeOk = before && entered === 1 && first.value === 1 && !first.done &&
    second.value === 41 && !second.done && third.value === 42 && third.done &&
    fourth.value === undefined && fourth.done;

var finallyValue = 0;
function* protectedYield() {
    try {
        yield 4;
    } finally {
        var injected = yield 5;
        finallyValue = injected;
    }
}
var protectedGenerator = protectedYield();
var protectedFirst = protectedGenerator.next();
var protectedSecond = protectedGenerator.next(10);
var protectedThird = protectedGenerator.next(77);
var finallyOk = protectedFirst.value === 4 && !protectedFirst.done &&
    protectedSecond.value === 5 && !protectedSecond.done && protectedThird.done &&
    protectedThird.value === undefined && finallyValue === 77;

exchangeOk && finallyOk;
"#;

#[test]
fn generator_return_slice_runs_for_every_dispatch_batch() {
    assert_generator_source::<1>(false);
    assert_generator_source::<2>(false);
    assert_generator_source::<4>(false);
    assert_generator_source::<8>(false);
    assert_generator_source::<16>(false);
}

#[test]
fn generator_state_and_arguments_survive_forced_major_collection() {
    assert_generator_source::<8>(true);
}

#[test]
fn generator_yield_and_next_value_run_for_every_dispatch_batch() {
    assert_generator_yield_source::<1>(false);
    assert_generator_yield_source::<2>(false);
    assert_generator_yield_source::<4>(false);
    assert_generator_yield_source::<8>(false);
    assert_generator_yield_source::<16>(false);
}

#[test]
fn generator_yield_state_survives_forced_major_collection() {
    assert_generator_yield_source::<8>(true);
}

/// Exercises repeated Fiber ownership transfer without growing the native Rust call stack.
#[test]
fn generator_large_resume_loop_uses_constant_native_stack() {
    let source = r#"
function* count(limit) {
    var index = 0;
    while (index < limit) {
        yield index;
        index = index + 1;
    }
    return index;
}
var generator = count(512);
var expected = 0;
var valid = true;
while (expected < 512) {
    var result = generator.next();
    valid = valid && !result.done && result.value === expected;
    expected = expected + 1;
}
var completed = generator.next();
valid && completed.done && completed.value === 512;
"#;
    let (_, outcome) = execute_generator_fixture(2_502, source);
    assert_eq!(outcome.as_immediate(), Some(Immediate::True));
}

/// Verifies exact suspended storage and constant-space completed `.next()` behavior at large argc.
#[test]
fn generator_large_activation_is_exact_then_released_before_repeated_next() {
    const ARGUMENT_COUNT: usize = 256;
    let arguments = (0..ARGUMENT_COUNT)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let suspended_source = format!(
        "function* many() {{ return arguments.length; }}\nvar generator = many({arguments});\ngenerator;"
    );
    let (mut isolate, suspended) = execute_generator_fixture(2_500, &suspended_source);
    assert_eq!(
        isolate
            .generator_retained_argument_capacity(suspended)
            .expect("suspended generator has inspectable retained arguments"),
        ARGUMENT_COUNT,
        "suspended-start backing is an exact boxed argument list"
    );

    let completed_source = format!(
        "function* many() {{ return arguments.length; }}\n\
         var generator = many({arguments});\n\
         var first = generator.next();\n\
         if (first.value !== {ARGUMENT_COUNT} || !first.done) {{ throw 'first'; }}\n\
         var index = 0;\n\
         while (index < 1024) {{\n\
             var next = generator.next();\n\
             if (next.value !== undefined || !next.done) {{ throw 'repeat'; }}\n\
             index = index + 1;\n\
         }}\n\
         generator;"
    );
    let (mut isolate, completed) = execute_generator_fixture(2_501, &completed_source);
    assert_eq!(
        isolate
            .generator_retained_argument_capacity(completed)
            .expect("completed generator remains inspectable"),
        0,
        "completed generator releases its argument-prefix root"
    );
}

/// Compiles and executes the complete return-only generator contract under one dispatch policy.
fn assert_generator_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_400 + N as u32),
                SourceName::new("generator-return-slice"),
                MediaType::JavaScript,
                Arc::from(GENERATOR_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generator fixture compiles");
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
                fuel: 100_000,
                quantum: 100_000,
            },
        )
        .expect("generator fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles and runs the complete ordinary-yield contract under one dispatch/collection policy.
fn assert_generator_yield_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_600 + N as u32),
                SourceName::new("generator-yield-slice"),
                MediaType::JavaScript,
                Arc::from(GENERATOR_YIELD_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generator yield fixture compiles");
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
                fuel: 200_000,
                quantum: 200_000,
            },
        )
        .expect("generator yield fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "yield dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes a generated stress fixture and returns both the isolate and its final generator value.
fn execute_generator_fixture(source_id: u32, source: &str) -> (Isolate, Value) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("generator-activation-stress"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("generator stress fixture compiles");
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 500_000,
                quantum: 500_000,
            },
        )
        .expect("generator stress fixture executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("generator stress fixture did not complete: {outcome:?}");
    };
    (isolate, value)
}
