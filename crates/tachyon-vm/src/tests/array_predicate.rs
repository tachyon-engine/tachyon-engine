use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_PREDICATE_SOURCE: &str = r#"
var context = {};
var receiver = [2, , 4];
Array.prototype[1] = 3;
var everyTrace = "";
var everyResult = receiver.every(function(value, index, array) {
  "use strict";
  everyTrace += value + ":" + index + ":" + (array === receiver) + ":" + (this === context) + ";";
  return value < 4;
}, context);
var someCount = 0;
var someResult = receiver.some(function(value) {
  someCount += 1;
  return value === 3;
});
var snapshot = [1, 2];
var snapshotCount = 0;
var snapshotResult = snapshot.every(function(value, index) {
  snapshotCount += 1;
  if (index === 0) snapshot[2] = 3;
  return true;
});
var order = "";
var badReceiver = {
  get length() { order += "l"; return 0; }
};
var badCallback = false;
try {
  Array.prototype.every.call(badReceiver, null);
} catch (error) {
  badCallback = error instanceof TypeError;
}
everyResult === false &&
everyTrace === "2:0:true:true;3:1:true:true;4:2:true:true;" &&
someResult === true && someCount === 2 &&
snapshotResult === true && snapshotCount === 2 &&
order === "l" && badCallback &&
[].every(function() { return false; }) === true &&
[].some(function() { return true; }) === false;
"#;

const ARRAY_PREDICATE_PROXY_SOURCE: &str = r#"
var trace = "";
var target = { 0: 5, length: 2 };
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
var marker = {};
var observed = false;
try {
  Array.prototype.some.call(proxy, function(value) {
    if (value === 5) throw marker;
    return false;
  });
} catch (error) {
  observed = error === marker;
}
observed && trace === "glength;h0;g0;";
"#;

#[test]
fn array_predicates_are_stable_for_every_dispatch_batch() {
    assert_array_predicate_batch::<1>();
    assert_array_predicate_batch::<2>();
    assert_array_predicate_batch::<4>();
    assert_array_predicate_batch::<8>();
    assert_array_predicate_batch::<16>();
}

#[test]
fn array_predicate_proxy_and_abrupt_paths_are_stable_for_every_dispatch_batch() {
    assert_array_predicate_proxy_batch::<1>();
    assert_array_predicate_proxy_batch::<2>();
    assert_array_predicate_proxy_batch::<4>();
    assert_array_predicate_proxy_batch::<8>();
    assert_array_predicate_proxy_batch::<16>();
}

#[test]
fn array_predicate_state_survives_forced_major_collections() {
    for (source, source_id) in [
        (ARRAY_PREDICATE_SOURCE, 1_345),
        (ARRAY_PREDICATE_PROXY_SOURCE, 1_346),
    ] {
        let module = compile_array_predicate_source(source, source_id);
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<8>(
                &module,
                ExecutionBudget {
                    fuel: 16_384,
                    quantum: 16_384,
                },
            )
            .expect("forced-major Array predicate fixture executes");
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
            "forced-major Array predicate fixture returned {outcome:?}"
        );
    }
}

/// Executes the ordinary every/some contracts under one interpreter dispatch batch size.
fn assert_array_predicate_batch<const N: usize>() {
    assert_array_predicate_source::<N>(ARRAY_PREDICATE_SOURCE, 1_340 + N as u32);
}

/// Executes Proxy observation and abrupt callback propagation under one dispatch batch size.
fn assert_array_predicate_proxy_batch<const N: usize>() {
    assert_array_predicate_source::<N>(ARRAY_PREDICATE_PROXY_SOURCE, 1_360 + N as u32);
}

/// Compiles and executes one predicate fixture with the selected dispatch monomorphization.
fn assert_array_predicate_source<const N: usize>(source: &str, source_id: u32) {
    let module = compile_array_predicate_source(source, source_id);
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("Array predicate fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one Array predicate fixture without coupling it to an isolate collection policy.
fn compile_array_predicate_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-predicate-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array predicate fixture compiles")
}
