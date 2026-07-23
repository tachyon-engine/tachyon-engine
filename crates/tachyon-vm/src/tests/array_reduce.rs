use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_REDUCE_SOURCE: &str = r#"
var receiver = [1, 2, 3];
var callbackContract = true;
var sum = receiver.reduce(function(accumulator, value, index, object) {
  "use strict";
  callbackContract = callbackContract && this === undefined && arguments.length === 4 && object === receiver;
  return accumulator + value + index - index;
}, 0);
var reverseTrace = "";
var reverse = receiver.reduceRight(function(accumulator, value, index, object) {
  reverseTrace += value;
  return accumulator + value;
}, 0);
var sparse = [, , 3];
Array.prototype[1] = 2;
var sparseCalls = 0;
var sparseResult = sparse.reduce(function(accumulator, value, index, object) {
  sparseCalls += 1;
  return accumulator === 2 && value === 3 && index === 2 && object === sparse ? 5 : 0;
});
delete Array.prototype[1];
var snapshot = [1, 2];
var snapshotCalls = 0;
var snapshotResult = snapshot.reduce(function(accumulator, value, index) {
  snapshotCalls += 1;
  if (index === 0) snapshot[2] = 3;
  return accumulator + value;
}, 0);
var emptyThrows = false;
try {
  [].reduce(function() {});
} catch (error) {
  emptyThrows = error instanceof TypeError;
}
var lengthTrace = "";
var generic = {
  0: 4,
  length: { valueOf: function() { lengthTrace += "l"; return 1; } }
};
var genericResult = Array.prototype.reduce.call(generic, function(accumulator, value) {
  return accumulator + value;
}, 0);
sum === 6 && callbackContract && reverse === 6 && reverseTrace === "321" &&
sparseResult === 5 && sparseCalls === 1 &&
snapshotResult === 3 && snapshotCalls === 2 && emptyThrows &&
genericResult === 4 && lengthTrace === "l" &&
[].reduce(function() { throw 1; }, undefined) === undefined &&
[].reduceRight(function() { throw 2; }, undefined) === undefined;
"#;

const ARRAY_REDUCE_PROXY_SOURCE: &str = r#"
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
var forward = Array.prototype.reduce.call(proxy, function(accumulator, value) {
  return accumulator + value;
});
var forwardTrace = trace;
trace = "";
var backward = Array.prototype.reduceRight.call(proxy, function(accumulator, value) {
  return accumulator + value;
}, 0);
var backwardTrace = trace;
var marker = {};
var abrupt = false;
try {
  [1].reduce(function() { throw marker; }, 0);
} catch (error) {
  abrupt = error === marker;
}
forward === 4 && backward === 4 && abrupt &&
forwardTrace === "glength;h0;g0;h1;h2;g2;" &&
backwardTrace === "glength;h2;g2;h1;h0;g0;";
"#;

const ARRAY_REDUCE_SAFE_INTEGER_SOURCE: &str = r#"
var object = { length: Number.MAX_SAFE_INTEGER };
object[Number.MAX_SAFE_INTEGER - 1] = 1;
object[Number.MAX_SAFE_INTEGER - 3] = 3;
var marker = {};
var trace = "";
var observed = false;
try {
  Array.prototype.reduceRight.call(object, function(accumulator, value, index) {
    trace += value + ":" + index + ";";
    if (value === 3) throw marker;
    return accumulator + value;
  }, 0);
} catch (error) {
  observed = error === marker;
}
observed && trace === "1:9007199254740990;3:9007199254740988;";
"#;

#[test]
fn array_reductions_are_stable_for_every_dispatch_batch() {
    assert_array_reduce_batch::<1>();
    assert_array_reduce_batch::<2>();
    assert_array_reduce_batch::<4>();
    assert_array_reduce_batch::<8>();
    assert_array_reduce_batch::<16>();
}

#[test]
fn array_reduction_proxy_paths_are_stable_for_every_dispatch_batch() {
    assert_array_reduce_proxy_batch::<1>();
    assert_array_reduce_proxy_batch::<2>();
    assert_array_reduce_proxy_batch::<4>();
    assert_array_reduce_proxy_batch::<8>();
    assert_array_reduce_proxy_batch::<16>();
}

#[test]
fn array_reduction_state_survives_forced_major_collections() {
    for (source, source_id) in [
        (ARRAY_REDUCE_SOURCE, 1_385),
        (ARRAY_REDUCE_PROXY_SOURCE, 1_386),
    ] {
        let module = compile_array_reduce_source(source, source_id);
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
            .expect("forced-major Array reduction fixture executes");
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
            "forced-major Array reduction fixture returned {outcome:?}"
        );
    }
}

#[test]
/// Uses a larger atom quota because each observed String index is intentionally materialized.
fn array_reduction_long_native_callback_loop_does_not_grow_the_rust_stack() {
    let module = compile_array_reduce_source(
        "var text = 'a'.repeat(20000); Array.prototype.reduce.call(text, Boolean, true) === true;",
        1_387,
    );
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(8 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("large-atom test isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 2_000_000,
                quantum: 16_384,
            },
        )
        .expect("long native Array reduction executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long native Array reduction returned {outcome:?}"
    );
}

#[test]
fn array_reduce_right_skips_proven_ordinary_holes_near_the_safe_integer_limit() {
    assert_array_reduce_source::<8>(ARRAY_REDUCE_SAFE_INTEGER_SOURCE, 1_388, 16_384);
}

/// Executes ordinary reduce/reduceRight semantics under one interpreter dispatch batch size.
fn assert_array_reduce_batch<const N: usize>() {
    assert_array_reduce_source::<N>(ARRAY_REDUCE_SOURCE, 1_380 + N as u32, 16_384);
}

/// Executes Proxy observation order under one interpreter dispatch batch size.
fn assert_array_reduce_proxy_batch<const N: usize>() {
    assert_array_reduce_source::<N>(ARRAY_REDUCE_PROXY_SOURCE, 1_400 + N as u32, 16_384);
}

/// Compiles and executes one reduction fixture with the selected dispatch monomorphization.
fn assert_array_reduce_source<const N: usize>(source: &str, source_id: u32, fuel: u64) {
    let module = compile_array_reduce_source(source, source_id);
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel,
                quantum: 16_384,
            },
        )
        .expect("Array reduction fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one Array reduction fixture without coupling it to an isolate collection policy.
fn compile_array_reduce_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-reduce-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array reduction fixture compiles")
}
