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
