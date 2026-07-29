use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{ForcedCollectionMode, SPAN_SIZE_BYTES};

use super::super::*;

const ASYNC_FROM_SYNC_PROTOCOL_SOURCE: &str = r#"
var result = "";
var index = 0;
var iterable = {};
iterable[Symbol.iterator] = function() {
    return {
        next: function() {
            if (index === 0) {
                index += 1;
                return {
                    done: false,
                    value: { then: function(resolve) { resolve(3); } }
                };
            }
            throw "boom";
        }
    };
};
async function collect() {
    try {
        for await (var value of iterable) result += value + "|";
    } catch (error) {
        result += "caught:" + error + "|";
    }
}
collect();
result;
"#;

const ASYNC_FROM_SYNC_CONSTRUCTOR_THROW_SOURCE: &str = r#"
var result = "";
async function consume() {
    var promise = Promise.resolve(0);
    Object.defineProperty(promise, "constructor", {
        get: function() { throw new Error(); }
    });
    result += "start|";
    for await (var value of [promise]);
    result += "never|";
}
Promise.resolve(0)
    .then(function() { result += "tick1|"; })
    .then(function() { result += "tick2|"; });
consume().catch(function() { result += "catch|"; });
result;
"#;

const ORDINARY_AWAIT_REJECTION_ORDER_SOURCE: &str = r#"
var result = "";
var rejected = Promise.reject(0);
Promise.resolve(0)
    .then(function() { result += "tick1|"; })
    .then(function() { result += "tick2|"; });
async function consume() {
    try {
        await rejected;
    } catch (error) {
        result += "catch|";
    }
}
consume();
result;
"#;

const ASYNC_FROM_SYNC_CLOSE_ON_REJECT_SOURCE: &str = r#"
var result = "";

function* generated() {
    try {
        yield Promise.reject("generator");
    } finally {
        result += "generator-close|";
    }
}

var iterable = {};
iterable[Symbol.iterator] = function() {
    return {
        next: function() {
            return { done: false, value: Promise.reject("object") };
        },
        return: function() {
            result += "object-close|";
            return {};
        }
    };
};

async function consume() {
    try {
        for await (var value of generated());
    } catch (error) {
        result += "caught:" + error + "|";
    }
    try {
        for await (var value of iterable);
    } catch (error) {
        result += "caught:" + error + "|";
    }
    result += "done|";
}
consume();
result;
"#;

#[test]
fn async_from_sync_assimilates_thenables_and_rejects_sync_throws() {
    assert_async_from_sync_source::<1>(ASYNC_FROM_SYNC_PROTOCOL_SOURCE, "3|caught:boom|", false);
    assert_async_from_sync_source::<2>(ASYNC_FROM_SYNC_PROTOCOL_SOURCE, "3|caught:boom|", false);
    assert_async_from_sync_source::<4>(ASYNC_FROM_SYNC_PROTOCOL_SOURCE, "3|caught:boom|", false);
    assert_async_from_sync_source::<8>(ASYNC_FROM_SYNC_PROTOCOL_SOURCE, "3|caught:boom|", false);
    assert_async_from_sync_source::<16>(ASYNC_FROM_SYNC_PROTOCOL_SOURCE, "3|caught:boom|", false);
    assert_async_from_sync_source::<8>(ASYNC_FROM_SYNC_PROTOCOL_SOURCE, "3|caught:boom|", true);
}

#[test]
fn async_from_sync_constructor_throw_preserves_microtask_order() {
    assert_async_from_sync_source::<8>(
        ASYNC_FROM_SYNC_CONSTRUCTOR_THROW_SOURCE,
        "start|tick1|tick2|catch|",
        false,
    );
}

#[test]
fn ordinary_await_rejection_preserves_promise_chain_order() {
    assert_async_from_sync_source::<8>(
        ORDINARY_AWAIT_REJECTION_ORDER_SOURCE,
        "tick1|catch|tick2|",
        false,
    );
}

#[test]
fn async_from_sync_rejection_close_runs_for_every_dispatch_batch() {
    assert_async_from_sync_rejection_close::<1>(false);
    assert_async_from_sync_rejection_close::<2>(false);
    assert_async_from_sync_rejection_close::<4>(false);
    assert_async_from_sync_rejection_close::<8>(false);
    assert_async_from_sync_rejection_close::<16>(false);
}

#[test]
fn async_from_sync_rejection_close_survives_forced_major_collection() {
    assert_async_from_sync_rejection_close::<8>(true);
}

/// Checks generator and ordinary-iterator close ownership across one dispatch configuration.
fn assert_async_from_sync_rejection_close<const N: usize>(forced_major: bool) {
    assert_async_from_sync_source::<N>(
        ASYNC_FROM_SYNC_CLOSE_ON_REJECT_SOURCE,
        "generator-close|caught:generator|object-close|caught:object|done|",
        forced_major,
    );
}

/// Runs one Async-from-Sync fixture until all promise jobs have reached a stable result trace.
fn assert_async_from_sync_source<const N: usize>(
    source: &'static str,
    expected: &str,
    forced_major: bool,
) {
    let compiler = Compiler;
    let setup = compiler
        .compile(
            SourceText::new(
                SourceId::new(3_700 + N as u32),
                SourceName::new("async-from-sync-protocol"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Async-from-Sync protocol fixture compiles");
    let assertion = compiler
        .compile(
            SourceText::new(
                SourceId::new(3_800 + N as u32),
                SourceName::new("async-from-sync-protocol-assertion"),
                MediaType::JavaScript,
                Arc::from("result;"),
            ),
            CompileOptions::default(),
        )
        .expect("Async-from-Sync assertion compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(55, 56)),
        HeapLimit::new(128 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("Async-from-Sync isolate initializes");
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
        .expect("Async-from-Sync setup executes");
    let outcome = isolate
        .execute_with_batch::<N>(
            &assertion,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("Async-from-Sync assertion executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("Async-from-Sync assertion did not complete: {outcome:?}");
    };
    let value = isolate
        .string_value_to_utf16(value)
        .expect("Async-from-Sync result is a string");
    assert_eq!(String::from_utf16(&value).unwrap(), expected);
}
