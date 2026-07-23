use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_FIND_SOURCE: &str = r#"
var source = [, 1, 2, 1];
var marker = {};
var forwardCalls = 0;
var found = source.find(function(value, index, receiver) {
  "use strict";
  forwardCalls += 1;
  return this === marker && receiver === source && value === 2 && index === 2;
}, marker);
var foundIndex = source.findIndex(function(value, index, receiver) {
  return receiver === source && value === 1 && index > 1;
});
var reverseCalls = 0;
var foundLast = source.findLast(function(value, index) {
  reverseCalls += 1;
  return value === 1;
});
var foundLastIndex = source.findLastIndex(function(value, index) {
  return value === undefined;
});

var holes = [, , ,];
var holeCalls = 0;
var holeResult = holes.find(function(value, index) {
  holeCalls += 1;
  return index === 1;
});
var holeIndexCalls = 0;
var holeIndex = holes.findIndex(function(value, index) {
  holeIndexCalls += 1;
  return index === 2;
});

var changed = [1, 2, 3];
var changedCalls = 0;
var changedResult = changed.find(function(value, index) {
  changedCalls += 1;
  if (index === 0) delete changed[1];
  if (index === 1 && value === undefined) changed[2] = 4;
  return value === 4;
});

found === 2 && forwardCalls === 3 && foundIndex === 3 &&
  foundLast === 1 && reverseCalls === 1 && foundLastIndex === 0 &&
  holeResult === undefined && holeCalls === 2 && holeIndex === 2 && holeIndexCalls === 3 &&
  changedResult === 4 && changedCalls === 3;
"#;

const ARRAY_FIND_PROXY_SOURCE: &str = r#"
var trace = "";
var hasCalls = 0;
var target = { 0: 1, 2: 3, length: 3 };
var proxy = new Proxy(target, {
  get: function(object, key, receiver) {
    trace += "g" + key + ";";
    return Reflect.get(object, key, receiver);
  },
  has: function(object, key) {
    hasCalls += 1;
    return key in object;
  }
});
var forward = Array.prototype.findIndex.call(proxy, function(value, index, receiver) {
  return receiver === proxy && index === 2;
});
var forwardTrace = trace;
trace = "";
var backward = Array.prototype.findLastIndex.call(proxy, function(value, index, receiver) {
  return receiver === proxy && index === 0;
});
var backwardTrace = trace;
forward === 2 && backward === 0 && hasCalls === 0 &&
  forwardTrace === "glength;g0;g1;g2;" &&
  backwardTrace === "glength;g2;g1;g0;";
"#;

const ARRAY_FIND_MAXIMUM_INDEX_SOURCE: &str = r#"
var object = { length: Number.MAX_VALUE };
var valueIndex = -1;
var indexIndex = -1;
var value = Array.prototype.findLast.call(object, function(element, index) {
  valueIndex = index;
  return true;
});
var result = Array.prototype.findLastIndex.call(object, function(element, index) {
  indexIndex = index;
  return true;
});
value === undefined && valueIndex === Number.MAX_SAFE_INTEGER - 1 &&
  result === Number.MAX_SAFE_INTEGER - 1 && indexIndex === Number.MAX_SAFE_INTEGER - 1;
"#;

#[test]
fn array_find_family_is_stable_for_every_dispatch_batch() {
    assert_array_find_source::<1>(ARRAY_FIND_SOURCE, 1_601, 16_384, false);
    assert_array_find_source::<2>(ARRAY_FIND_SOURCE, 1_602, 16_384, false);
    assert_array_find_source::<4>(ARRAY_FIND_SOURCE, 1_604, 16_384, false);
    assert_array_find_source::<8>(ARRAY_FIND_SOURCE, 1_608, 16_384, false);
    assert_array_find_source::<16>(ARRAY_FIND_SOURCE, 1_616, 16_384, false);
}

#[test]
fn array_find_proxy_get_order_is_stable_for_every_dispatch_batch() {
    assert_array_find_source::<1>(ARRAY_FIND_PROXY_SOURCE, 1_621, 16_384, false);
    assert_array_find_source::<2>(ARRAY_FIND_PROXY_SOURCE, 1_622, 16_384, false);
    assert_array_find_source::<4>(ARRAY_FIND_PROXY_SOURCE, 1_624, 16_384, false);
    assert_array_find_source::<8>(ARRAY_FIND_PROXY_SOURCE, 1_628, 16_384, false);
    assert_array_find_source::<16>(ARRAY_FIND_PROXY_SOURCE, 1_636, 16_384, false);
}

#[test]
fn array_find_state_survives_forced_major_collections() {
    assert_array_find_source::<8>(ARRAY_FIND_SOURCE, 1_640, 16_384, true);
    assert_array_find_source::<8>(ARRAY_FIND_PROXY_SOURCE, 1_641, 16_384, true);
}

#[test]
fn array_find_last_preserves_the_maximum_safe_index() {
    assert_array_find_source::<8>(ARRAY_FIND_MAXIMUM_INDEX_SOURCE, 1_642, 16_384, false);
}

/// Compiles and executes one find-family fixture with the selected collection policy.
fn assert_array_find_source<const N: usize>(
    source: &str,
    source_id: u32,
    fuel: u64,
    forced_major: bool,
) {
    let module = compile_array_find_source(source, source_id);
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
        .expect("Array find fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one Array find fixture without coupling it to an isolate collection policy.
fn compile_array_find_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-find-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array find fixture compiles")
}
