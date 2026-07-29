use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{ForcedCollectionMode, SPAN_SIZE_BYTES};

use super::super::*;

const ASYNC_FUNCTION_SOURCE: &str = r#"
var trace = "";

async function immediate(value) {
    trace += "body|";
    return value + 1;
}
var immediatePromise = immediate(4);
trace += "caller|";
immediatePromise.then(function(value) { trace += "return:" + value + "|"; });

async function paused() {
    trace += "before|";
    var value = await 7;
    trace += "after:" + value + "|";
    return value + 1;
}
paused().then(function(value) { trace += "paused:" + value + "|"; });

var marker = {};
async function rejected() {
    try {
        await Promise.reject(marker);
        trace += "bad|";
    } catch (error) {
        trace += error === marker ? "caught|" : "wrong|";
    } finally {
        trace += "finally|";
    }
    return 9;
}
rejected().then(function(value) { trace += "rejected:" + value + "|"; });

var arrow = async (value) => await value;
arrow(3).then(function(value) { trace += "arrow:" + value + "|"; });

trace;
"#;

const ASYNC_FUNCTION_ASSERTION: &str = r#"
trace;
"#;

const NAMED_ASYNC_PARAMETER_SOURCE: &str = r#"
var result = "";
var original;
original = async function self(a = 3, b = a, c = self) {
    result += a + ":" + b + ":" + (c === original) + "|";
    self = 1;
    result += (self === original) + "|";
    (() => { self = 2; })();
    result += (self === original) + "|";
};
original(undefined);
result;
"#;

const ARROW_LEXICAL_ARGUMENTS_SOURCE: &str = r#"
var result = "";
function outer(value) {
    var identity = arguments;
    return () => arguments === identity && arguments[0] === value;
}
var escaped = outer(7);
result += escaped() + "|";
var escapedAsync;
async function asyncOuter(value) {
    var identity = arguments;
    escapedAsync = async () => {
        result += (arguments === identity && arguments[0] === value) + "|";
    };
}
asyncOuter(9);
escapedAsync();
result;
"#;

const FOR_AWAIT_ASYNC_GENERATOR_SOURCE: &str = r#"
var result = "";
async function* values() { yield 1; yield 2; }
async function collect() {
    for await (var value of values()) result += value + "|";
    result += "done|";
}
collect();
result;
"#;

const FOR_AWAIT_SYNC_ITERABLE_SOURCE: &str = r#"
var result = "";
async function collect() {
    for await (var value of [Promise.resolve(1), 2]) result += value + "|";
    result += "done|";
}
collect();
result;
"#;

#[test]
fn async_function_await_runs_for_every_dispatch_batch() {
    assert_async_function_source::<1>(false);
    assert_async_function_source::<2>(false);
    assert_async_function_source::<4>(false);
    assert_async_function_source::<8>(false);
    assert_async_function_source::<16>(false);
}

#[test]
fn async_function_state_survives_forced_major_collection() {
    assert_async_function_source::<1>(true);
    assert_async_function_source::<2>(true);
    assert_async_function_source::<4>(true);
    assert_async_function_source::<8>(true);
    assert_async_function_source::<16>(true);
}

#[test]
fn named_async_parameter_environment_works_for_every_dispatch_batch() {
    assert_named_async_parameters::<1>(false);
    assert_named_async_parameters::<2>(false);
    assert_named_async_parameters::<4>(false);
    assert_named_async_parameters::<8>(false);
    assert_named_async_parameters::<16>(false);
    assert_named_async_parameters::<8>(true);
}

#[test]
fn arrow_lexical_arguments_escape_for_every_dispatch_batch() {
    assert_arrow_lexical_arguments::<1>(false);
    assert_arrow_lexical_arguments::<2>(false);
    assert_arrow_lexical_arguments::<4>(false);
    assert_arrow_lexical_arguments::<8>(false);
    assert_arrow_lexical_arguments::<16>(false);
    assert_arrow_lexical_arguments::<8>(true);
}

#[test]
fn for_await_consumes_async_generators_for_every_dispatch_batch() {
    assert_for_await_async_generator::<1>(false);
    assert_for_await_async_generator::<2>(false);
    assert_for_await_async_generator::<4>(false);
    assert_for_await_async_generator::<8>(false);
    assert_for_await_async_generator::<16>(false);
    assert_for_await_async_generator::<8>(true);
}

#[test]
fn for_await_consumes_sync_iterables_for_every_dispatch_batch() {
    assert_for_await_sync_iterable::<1>(false);
    assert_for_await_sync_iterable::<2>(false);
    assert_for_await_sync_iterable::<4>(false);
    assert_for_await_sync_iterable::<8>(false);
    assert_for_await_sync_iterable::<16>(false);
}

#[test]
fn for_await_sync_iterable_survives_forced_major_collection() {
    assert_for_await_sync_iterable::<8>(true);
}

/// Runs an Async-from-Sync loop through promise assimilation and the shared body.
fn assert_for_await_sync_iterable<const N: usize>(forced_major: bool) {
    let compiler = Compiler;
    let setup = compiler
        .compile(
            SourceText::new(
                SourceId::new(3_500 + N as u32),
                SourceName::new("for-await-sync-iterable"),
                MediaType::JavaScript,
                Arc::from(FOR_AWAIT_SYNC_ITERABLE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("sync for-await fixture compiles");
    let assertion = compiler
        .compile(
            SourceText::new(
                SourceId::new(3_600 + N as u32),
                SourceName::new("for-await-sync-assertion"),
                MediaType::JavaScript,
                Arc::from("result;"),
            ),
            CompileOptions::default(),
        )
        .expect("sync for-await assertion compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(53, 54)),
        HeapLimit::new(128 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("sync for-await isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    isolate
        .execute_with_batch::<N>(
            &setup,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("sync for-await setup executes");
    let outcome = isolate
        .execute_with_batch::<N>(
            &assertion,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("sync for-await assertion executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("sync for-await assertion did not complete: {outcome:?}");
    };
    let value = isolate
        .string_value_to_utf16(value)
        .expect("sync for-await trace is a string");
    assert_eq!(String::from_utf16(&value).unwrap(), "1|2|done|");
}

/// Runs one async-generator consumer until queued promise reactions finish.
fn assert_for_await_async_generator<const N: usize>(forced_major: bool) {
    let compiler = Compiler;
    let setup = compiler
        .compile(
            SourceText::new(
                SourceId::new(3_300 + N as u32),
                SourceName::new("for-await-async-generator"),
                MediaType::JavaScript,
                Arc::from(FOR_AWAIT_ASYNC_GENERATOR_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("for-await fixture compiles");
    let assertion = compiler
        .compile(
            SourceText::new(
                SourceId::new(3_400 + N as u32),
                SourceName::new("for-await-assertion"),
                MediaType::JavaScript,
                Arc::from("result;"),
            ),
            CompileOptions::default(),
        )
        .expect("for-await assertion compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(51, 52)),
        HeapLimit::new(128 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("for-await isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    isolate
        .execute_with_batch::<N>(
            &setup,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("for-await setup executes");
    let outcome = isolate
        .execute_with_batch::<N>(
            &assertion,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("for-await assertion executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("for-await assertion did not complete: {outcome:?}");
    };
    let value = isolate
        .string_value_to_utf16(value)
        .expect("for-await trace is a string");
    assert_eq!(String::from_utf16(&value).unwrap(), "1|2|done|");
}

/// Runs one complete async-function lifecycle under a dispatch and collection policy.
fn assert_async_function_source<const N: usize>(forced_major: bool) {
    let compiler = Compiler;
    let setup = compiler
        .compile(
            SourceText::new(
                SourceId::new(2_900 + N as u32),
                SourceName::new("async-function-await"),
                MediaType::JavaScript,
                Arc::from(ASYNC_FUNCTION_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("async function fixture compiles");
    let assertion = compiler
        .compile(
            SourceText::new(
                SourceId::new(3_000 + N as u32),
                SourceName::new("async-function-assertion"),
                MediaType::JavaScript,
                Arc::from(ASYNC_FUNCTION_ASSERTION),
            ),
            CompileOptions::default(),
        )
        .expect("async function assertion compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(31, 32)),
        HeapLimit::new(128 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("async function isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    isolate
        .execute_with_batch::<N>(
            &setup,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("async function setup executes");
    let outcome = isolate
        .execute_with_batch::<N>(
            &assertion,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("async function assertion executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("async function trace did not complete: {outcome:?}");
    };
    let trace = isolate
        .string_value_to_utf16(value)
        .expect("async function trace is a string");
    assert_eq!(
        String::from_utf16(&trace).expect("trace is valid UTF-16"),
        "body|caller|before|return:5|after:7|caught|finally|paused:8|rejected:9|arrow:3|",
        "async function batch {N}, forced_major={forced_major}"
    );
}

/// Verifies named-function and parameter environments remain distinct across async suspension.
fn assert_named_async_parameters<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(3_100 + N as u32),
                SourceName::new("named-async-parameters"),
                MediaType::JavaScript,
                Arc::from(NAMED_ASYNC_PARAMETER_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("named async parameter fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(41, 42)),
        HeapLimit::new(128 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("named async parameter isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("named async parameter fixture executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("named async parameter fixture did not complete: {outcome:?}");
    };
    let result = isolate
        .string_value_to_utf16(value)
        .expect("named async parameter result is a string");
    assert_eq!(String::from_utf16(&result).unwrap(), "3:3:true|true|true|");
}

/// Verifies escaped ordinary and async arrows retain the owner activation's arguments object.
fn assert_arrow_lexical_arguments<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(3_200 + N as u32),
                SourceName::new("arrow-lexical-arguments"),
                MediaType::JavaScript,
                Arc::from(ARROW_LEXICAL_ARGUMENTS_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("arrow lexical arguments fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(43, 44)),
        HeapLimit::new(128 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("arrow lexical arguments isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("arrow lexical arguments fixture executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("arrow lexical arguments fixture did not complete: {outcome:?}");
    };
    let result = isolate
        .string_value_to_utf16(value)
        .expect("arrow lexical arguments result is a string");
    assert_eq!(String::from_utf16(&result).unwrap(), "true|true|");
}
