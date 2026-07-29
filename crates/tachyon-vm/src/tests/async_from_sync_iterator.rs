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

const ASYNC_FROM_SYNC_MISSING_THROW_SETUP: &str = r#"
var result = "";
var getterMarker = {};
var callMarker = {};
var throwGetterMarker = {};

var objectIterator = {
    next: function() { return { done: false, value: 1 }; },
    return: function() { result += "object-return:" + arguments.length + "|"; return {}; }
};
var missingIterator = {
    next: function() { return { done: false, value: 1 }; },
    get return() { result += "missing-get|"; return undefined; }
};
var getterThrowIterator = {
    next: function() { return { done: false, value: 1 }; },
    get return() { result += "getter-throw|"; throw getterMarker; }
};
var callThrowIterator = {
    next: function() { return { done: false, value: 1 }; },
    return: function() { result += "call-throw|"; throw callMarker; }
};
var primitiveIterator = {
    next: function() { return { done: false, value: 1 }; },
    return: function() { result += "primitive-return|"; return 1; }
};
var nonCallableThrowIterator = {
    next: function() { return { done: false, value: 1 }; },
    throw: 1,
    return: function() { result += "noncallable-unexpected-close|"; return {}; }
};
var throwGetterIterator = {
    next: function() { return { done: false, value: 1 }; },
    get throw() { result += "throw-getter|"; throw throwGetterMarker; },
    return: function() { result += "getter-unexpected-close|"; return {}; }
};

function* closeGenerator() {
    try { yield 1; }
    finally { result += "generator-finally|"; }
}
var closeGeneratorInstance = closeGenerator();
closeGeneratorInstance.next();
var generatorIterator = {
    next: function() { return closeGeneratorInstance.next(); },
    return: closeGeneratorInstance.return.bind(closeGeneratorInstance)
};

var objectNext = objectIterator.next;
var missingNext = missingIterator.next;
var getterThrowNext = getterThrowIterator.next;
var callThrowNext = callThrowIterator.next;
var primitiveNext = primitiveIterator.next;
var nonCallableThrowNext = nonCallableThrowIterator.next;
var throwGetterNext = throwGetterIterator.next;
var generatorNext = generatorIterator.next;
result;
"#;

const ASYNC_FROM_SYNC_MISSING_THROW_ACTION: &str = r#"
async function expectType(label, wrapper) {
    try {
        await wrapper.throw("sentinel");
        result += label + ":unexpected|";
    } catch (error) {
        result += label + ":" + (error instanceof TypeError) + "|";
    }
}
async function runMissingThrowClose() {
    await expectType("object", objectWrapper);
    await expectType("missing", missingWrapper);
    try { await getterThrowWrapper.throw("sentinel"); }
    catch (error) { result += "getter:" + (error === getterMarker) + "|"; }
    try { await callThrowWrapper.throw("sentinel"); }
    catch (error) { result += "call:" + (error === callMarker) + "|"; }
    await expectType("primitive", primitiveWrapper);
    await expectType("noncallable", nonCallableThrowWrapper);
    try { await throwGetterWrapper.throw("sentinel"); }
    catch (error) { result += "throw-getter:" + (error === throwGetterMarker) + "|"; }
    await expectType("generator", generatorWrapper);
    result += "done|";
}
runMissingThrowClose();
result;
"#;

const ASYNC_FROM_SYNC_MISSING_THROW_EXPECTED: &str = concat!(
    "object-return:0|object:true|",
    "missing-get|missing:true|",
    "getter-throw|getter:true|",
    "call-throw|call:true|",
    "primitive-return|primitive:true|",
    "noncallable:true|",
    "throw-getter|throw-getter:true|",
    "generator-finally|generator:true|done|",
);

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

#[test]
fn async_from_sync_missing_throw_close_runs_for_every_dispatch_batch() {
    assert_async_from_sync_missing_throw_close::<1>(false);
    assert_async_from_sync_missing_throw_close::<2>(false);
    assert_async_from_sync_missing_throw_close::<4>(false);
    assert_async_from_sync_missing_throw_close::<8>(false);
    assert_async_from_sync_missing_throw_close::<16>(false);
}

#[test]
fn async_from_sync_missing_throw_close_survives_forced_major_collection() {
    assert_async_from_sync_missing_throw_close::<8>(true);
}

/// Checks generator and ordinary-iterator close ownership across one dispatch configuration.
fn assert_async_from_sync_rejection_close<const N: usize>(forced_major: bool) {
    assert_async_from_sync_source::<N>(
        ASYNC_FROM_SYNC_CLOSE_ON_REJECT_SOURCE,
        "generator-close|caught:generator|object-close|caught:object|done|",
        forced_major,
    );
}

/// Publishes internal wrappers so missing-throw cleanup can be tested without async yield-star.
fn assert_async_from_sync_missing_throw_close<const N: usize>(forced_major: bool) {
    let compiler = Compiler;
    let setup = compile_async_from_sync_fixture(
        &compiler,
        ASYNC_FROM_SYNC_MISSING_THROW_SETUP,
        3_900 + N as u32,
        "async-from-sync-missing-throw-setup",
    );
    let action = compile_async_from_sync_fixture(
        &compiler,
        ASYNC_FROM_SYNC_MISSING_THROW_ACTION,
        4_000 + N as u32,
        "async-from-sync-missing-throw-action",
    );
    let assertion = compile_async_from_sync_fixture(
        &compiler,
        "result;",
        4_100 + N as u32,
        "async-from-sync-missing-throw-assertion",
    );
    let mut isolate = async_from_sync_isolate(forced_major);
    execute_async_from_sync_fixture::<N>(&mut isolate, &setup, "missing-throw setup");
    for (iterator, next, wrapper) in [
        ("objectIterator", "objectNext", "objectWrapper"),
        ("missingIterator", "missingNext", "missingWrapper"),
        (
            "getterThrowIterator",
            "getterThrowNext",
            "getterThrowWrapper",
        ),
        ("callThrowIterator", "callThrowNext", "callThrowWrapper"),
        ("primitiveIterator", "primitiveNext", "primitiveWrapper"),
        (
            "nonCallableThrowIterator",
            "nonCallableThrowNext",
            "nonCallableThrowWrapper",
        ),
        (
            "throwGetterIterator",
            "throwGetterNext",
            "throwGetterWrapper",
        ),
        ("generatorIterator", "generatorNext", "generatorWrapper"),
    ] {
        publish_async_from_sync_wrapper(&mut isolate, iterator, next, wrapper);
    }
    execute_async_from_sync_fixture::<N>(&mut isolate, &action, "missing-throw action");
    let outcome =
        execute_async_from_sync_fixture::<N>(&mut isolate, &assertion, "missing-throw assertion");
    assert_async_from_sync_string(
        &mut isolate,
        outcome,
        ASYNC_FROM_SYNC_MISSING_THROW_EXPECTED,
    );
}

/// Creates and publishes one branded Async-from-Sync wrapper from existing global values.
fn publish_async_from_sync_wrapper(
    isolate: &mut Isolate,
    iterator_name: &str,
    next_name: &str,
    wrapper_name: &str,
) {
    let iterator = async_from_sync_global(isolate, iterator_name);
    let next = async_from_sync_global(isolate, next_name);
    let wrapper = isolate
        .create_async_from_sync_iterator(iterator, next)
        .expect("Async-from-Sync wrapper allocates");
    let atom = isolate
        .atoms
        .try_intern(JsString::try_from_str(wrapper_name).unwrap())
        .expect("wrapper name interns");
    isolate
        .realm
        .set(atom, wrapper)
        .expect("wrapper global publishes");
}

/// Reads one initialized var binding from the active Realm.
fn async_from_sync_global(isolate: &mut Isolate, name: &str) -> Value {
    let atom = isolate
        .atoms
        .try_intern(JsString::try_from_str(name).unwrap())
        .expect("global name interns");
    isolate
        .realm
        .resolve(atom)
        .and_then(|slot| isolate.realm.get_slot(slot))
        .expect("fixture global exists")
}

/// Runs one Async-from-Sync fixture until all promise jobs have reached a stable result trace.
fn assert_async_from_sync_source<const N: usize>(
    source: &'static str,
    expected: &str,
    forced_major: bool,
) {
    let compiler = Compiler;
    let setup = compile_async_from_sync_fixture(
        &compiler,
        source,
        3_700 + N as u32,
        "async-from-sync-protocol",
    );
    let assertion = compile_async_from_sync_fixture(
        &compiler,
        "result;",
        3_800 + N as u32,
        "async-from-sync-protocol-assertion",
    );
    let mut isolate = async_from_sync_isolate(forced_major);
    execute_async_from_sync_fixture::<N>(&mut isolate, &setup, "protocol setup");
    let outcome =
        execute_async_from_sync_fixture::<N>(&mut isolate, &assertion, "protocol assertion");
    assert_async_from_sync_string(&mut isolate, outcome, expected);
}

/// Compiles one immutable script used by an Async-from-Sync protocol fixture.
fn compile_async_from_sync_fixture(
    compiler: &Compiler,
    source: &'static str,
    source_id: u32,
    name: &'static str,
) -> CompiledModule {
    compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new(name),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Async-from-Sync fixture compiles")
}

/// Creates the shared bounded isolate and optionally forces every allocation through major GC.
fn async_from_sync_isolate(forced_major: bool) -> Isolate {
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
}

/// Executes one fixture with enough fuel to drain every Promise job it schedules.
fn execute_async_from_sync_fixture<const N: usize>(
    isolate: &mut Isolate,
    module: &CompiledModule,
    label: &str,
) -> RunOutcome {
    isolate
        .execute_with_batch::<N>(
            module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .unwrap_or_else(|error| panic!("Async-from-Sync {label} executes: {error:?}"))
}

/// Converts the completed assertion value and compares its exact observable trace.
fn assert_async_from_sync_string(isolate: &mut Isolate, outcome: RunOutcome, expected: &str) {
    let RunOutcome::Completed(value) = outcome else {
        panic!("Async-from-Sync assertion did not complete: {outcome:?}");
    };
    let value = isolate
        .string_value_to_utf16(value)
        .expect("Async-from-Sync result is a string");
    assert_eq!(String::from_utf16(&value).unwrap(), expected);
}
