use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_SEARCH_SOURCE: &str = r#"
var object = {};
var other = {};
var values = [0, NaN, object, 0, object];
var strict = values.indexOf(NaN) === -1 &&
  values.indexOf(-0) === 0 && values.lastIndexOf(+0) === 3 &&
  values.indexOf(object) === 2 && values.indexOf(other) === -1;

var sparse = [, , 3];
Array.prototype[1] = 2;
var inherited = sparse.indexOf(2) === 1 && sparse.lastIndexOf(2) === 1;
delete Array.prototype[1];

Object.defineProperty(Object.prototype, "0", {
  get: function() { return false; },
  configurable: true
});
var ownOverride = { 0: true, 1: 1, length: 2 };
var overridesInheritedAccessor = Array.prototype.indexOf.call(ownOverride, true) === 0 &&
  Array.prototype.lastIndexOf.call(ownOverride, true) === 0;
delete Object.prototype["0"];

var omitted = [0, 1].lastIndexOf(1) === 1;
var explicitUndefined = [0, 1].lastIndexOf(1, undefined) === -1;
var boundaries = values.indexOf(0, Infinity) === -1 &&
  values.indexOf(0, -Infinity) === 0 &&
  values.lastIndexOf(0, Infinity) === 3 &&
  values.lastIndexOf(0, -Infinity) === -1 &&
  values.indexOf(object, "1.9") === 2 &&
  values.lastIndexOf(object, -1.2) === 4 &&
  values.indexOf(0, false) === 0;

var trace = "";
var fromIndex = { valueOf: function() { trace += "i"; return 1; } };
var generic = {
  1: 7,
  get length() { trace += "l"; return 2; }
};
var converted = Array.prototype.indexOf.call(generic, 7, fromIndex) === 1 && trace === "li";
var emptyIndexCalls = 0;
var emptyFrom = { valueOf: function() { emptyIndexCalls += 1; return 0; } };
var empty = Array.prototype.indexOf.call({ length: 0 }, 1, emptyFrom) === -1 &&
  emptyIndexCalls === 0;
var marker = {};
var abrupt = false;
try {
  [1].indexOf(1, { valueOf: function() { throw marker; } });
} catch (error) {
  abrupt = error === marker;
}

var includesTrace = "";
var includesTarget = { 0: undefined, 1: NaN, 2: -0, length: 3 };
var includesProxy = new Proxy(includesTarget, {
  get: function(object, key, receiver) {
    includesTrace += "g" + key + ";";
    return Reflect.get(object, key, receiver);
  },
  has: function() { includesTrace += "unexpected-has;"; return true; }
});
var includes = Array.prototype.includes.call(includesProxy, NaN, {
  valueOf: function() { includesTrace += "i;"; return 0; }
});
var includesDirectGet = includes && includesTrace === "glength;i;g0;g1;" &&
  [1].includes(1, Infinity) === false && [,].includes(undefined) &&
  [-0].includes(+0);

strict && inherited && overridesInheritedAccessor && omitted && explicitUndefined && boundaries &&
  converted && empty && abrupt && includesDirectGet;
"#;

const ARRAY_INCLUDES_GETTER_SOURCE: &str = r#"
var reads = 0;
var object = {
  length: 2,
  get 0() { reads += 1; return reads; },
  get 1() { reads += 1; return reads; }
};
var found = Array.prototype.includes.call(object, 2);
var emptyConversions = 0;
var empty = Array.prototype.includes.call(
  { length: 0 },
  1,
  { valueOf: function() { emptyConversions += 1; return 0; } }
);
found && reads === 2 && empty === false && emptyConversions === 0;
"#;

const ARRAY_INCLUDES_LONG_SOURCE: &str = r#"
var object = { length: 20000, 19999: 7 };
Array.prototype.includes.call(object, 7) &&
  !Array.prototype.includes.call(object, 8);
"#;

const ARRAY_SEARCH_PROXY_SOURCE: &str = r#"
var trace = "";
var target = { 0: 1, 2: 3, length: 3 };
var proxy = new Proxy(target, {
  get: function(object, key, receiver) {
    trace += "g" + key + ";";
    return Reflect.get(object, key, receiver);
  },
  has: function(object, key) {
    trace += "h" + key + ";";
    return key in object;
  }
});
var forward = Array.prototype.indexOf.call(proxy, 3);
var forwardTrace = trace;
trace = "";
var backward = Array.prototype.lastIndexOf.call(proxy, 1);
var backwardTrace = trace;
forward === 2 && backward === 0 &&
  forwardTrace === "glength;h0;g0;h1;h2;g2;" &&
  backwardTrace === "glength;h2;g2;h1;h0;g0;";
"#;

const ARRAY_SEARCH_SAFE_INTEGER_SOURCE: &str = r#"
var length = Number.MAX_SAFE_INTEGER;
var object = { length: length };
object[length - 3] = 3;
object[length - 1] = 1;
Array.prototype.indexOf.call(object, 3, length - 4) === length - 3 &&
  Array.prototype.lastIndexOf.call(object, 3) === length - 3 &&
  Array.prototype.indexOf.call(object, 9, length - 4) === -1 &&
  Array.prototype.lastIndexOf.call(object, 9) === -1;
"#;

#[test]
fn array_searches_are_stable_for_every_dispatch_batch() {
    assert_array_search_batch::<1>();
    assert_array_search_batch::<2>();
    assert_array_search_batch::<4>();
    assert_array_search_batch::<8>();
    assert_array_search_batch::<16>();
}

#[test]
fn array_search_proxy_paths_are_stable_for_every_dispatch_batch() {
    assert_array_search_source::<1>(ARRAY_SEARCH_PROXY_SOURCE, 1_511, 16_384, false);
    assert_array_search_source::<2>(ARRAY_SEARCH_PROXY_SOURCE, 1_512, 16_384, false);
    assert_array_search_source::<4>(ARRAY_SEARCH_PROXY_SOURCE, 1_514, 16_384, false);
    assert_array_search_source::<8>(ARRAY_SEARCH_PROXY_SOURCE, 1_518, 16_384, false);
    assert_array_search_source::<16>(ARRAY_SEARCH_PROXY_SOURCE, 1_526, 16_384, false);
}

#[test]
fn array_search_state_survives_forced_major_collections() {
    assert_array_search_source::<8>(ARRAY_SEARCH_SOURCE, 1_530, 16_384, true);
    assert_array_search_source::<8>(ARRAY_SEARCH_PROXY_SOURCE, 1_531, 16_384, true);
    assert_array_search_source::<8>(ARRAY_INCLUDES_GETTER_SOURCE, 1_533, 16_384, true);
}

#[test]
fn array_search_skips_proven_holes_near_the_safe_integer_limit() {
    assert_array_search_source::<8>(ARRAY_SEARCH_SAFE_INTEGER_SOURCE, 1_532, 32_768, false);
}

#[test]
/// Uses a larger atom quota because generic indexed Gets materialize property keys.
fn array_includes_long_sync_scan_does_not_grow_the_rust_stack() {
    let module = compile_array_search_source(ARRAY_INCLUDES_LONG_SOURCE, 1_534);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(32 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 32_768).with_max_shapes(32_768),
    ))
    .expect("large-atom includes isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("long synchronous includes executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long synchronous includes returned {outcome:?}"
    );
}

/// Executes ordinary search semantics under one interpreter dispatch batch size.
fn assert_array_search_batch<const N: usize>() {
    assert_array_search_source::<N>(ARRAY_SEARCH_SOURCE, 1_480 + N as u32, 16_384, false);
}

/// Compiles and executes one search fixture with the selected collection policy.
fn assert_array_search_source<const N: usize>(
    source: &str,
    source_id: u32,
    fuel: u64,
    forced_major: bool,
) {
    let module = compile_array_search_source(source, source_id);
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
                fuel,
                quantum: 16_384,
            },
        )
        .expect("Array search fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one Array search fixture without coupling it to an isolate collection policy.
fn compile_array_search_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-search-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array search fixture compiles")
}
