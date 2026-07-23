use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_INSERT_SOURCE: &str = r#"
var values = [2, 3];
var pushed = values.push(4, 5);
var unshifted = values.unshift(0, 1);
var denseOk = pushed === 4 && unshifted === 6 && values.length === 6 &&
  values[0] === 0 && values[1] === 1 && values[2] === 2 && values[5] === 5;

var sparse = [, 7, , 9];
var sparseLength = sparse.unshift(4, 5);
var sparseOk = sparseLength === 6 && sparse[0] === 4 && sparse[1] === 5 &&
  !(2 in sparse) && sparse[3] === 7 && !(4 in sparse) && sparse[5] === 9;

var trace = "";
var empty = {
  get length() { trace += "g"; return 0; },
  set length(value) { trace += "s" + value; }
};
var noItems = Array.prototype.unshift.call(empty);
var zeroOk = noItems === 0 && trace === "gs0";

var boxed = Array.prototype.push.call(true, 1);
denseOk && sparseOk && zeroOk && boxed === 1;
"#;

const ARRAY_INSERT_PROXY_SOURCE: &str = r#"
var trace = "";
var target = { 0: "a", 2: "c", length: 3 };
var proxy = new Proxy(target, {
  get: function(object, key, receiver) {
    trace += "g" + key + ";";
    return Reflect.get(object, key, receiver);
  },
  has: function(object, key) {
    trace += "h" + key + ";";
    return key in object;
  },
  set: function(object, key, value, receiver) {
    trace += "s" + key + ";";
    object[key] = value;
    return true;
  },
  deleteProperty: function(object, key) {
    trace += "d" + key + ";";
    delete object[key];
    return true;
  }
});
var length = Array.prototype.unshift.call(proxy, "x", "y");
var unshiftTrace = trace;
trace = "";
var pushed = Array.prototype.push.call(proxy, "z");
length === 5 && pushed === 6 && target.length === 6 && target[0] === "x" &&
  target[1] === "y" && target[2] === "a" && !(3 in target) && target[4] === "c" &&
  target[5] === "z" &&
  unshiftTrace === "glength;h2;g2;s4;h1;d3;h0;g0;s2;s0;s1;slength;" &&
  trace === "glength;s5;slength;";
"#;

const ARRAY_INSERT_GC_SOURCE: &str = r#"
var first = { id: 1 };
var second = { id: 2 };
var trace = "";
var source = {
  get 0() { return first; },
  get length() { return { valueOf: function() { return 1; } }; },
  set 1(value) { trace += "m" + value.id; },
  set 0(value) { trace += "a" + value.id; },
  set length(value) { trace += "l" + value; }
};
var result = Array.prototype.unshift.call(source, second);
result === 2 && first.id === 1 && second.id === 2 && trace === "m1a2l2";
"#;

const ARRAY_INSERT_OVERFLOW_SOURCE: &str = r#"
var threwPush = false;
var threwUnshift = false;
try {
  Array.prototype.push.call({ length: 9007199254740991 }, 1);
} catch (error) {
  threwPush = error instanceof TypeError;
}
try {
  Array.prototype.unshift.call({ length: 9007199254740991 }, 1);
} catch (error) {
  threwUnshift = error instanceof TypeError;
}
threwPush && threwUnshift;
"#;

const ARRAY_INSERT_LONG_SOURCE: &str = r#"
var sparse = [];
sparse.length = 20000;
sparse[19999] = 7;
var length = sparse.unshift(3);
length === 20001 && sparse[0] === 3 && sparse[20000] === 7 && !(19999 in sparse);
"#;

#[test]
fn array_insert_is_stable_for_every_dispatch_batch() {
    assert_array_insert_source::<1>(ARRAY_INSERT_SOURCE, 1_901, false);
    assert_array_insert_source::<2>(ARRAY_INSERT_SOURCE, 1_902, false);
    assert_array_insert_source::<4>(ARRAY_INSERT_SOURCE, 1_904, false);
    assert_array_insert_source::<8>(ARRAY_INSERT_SOURCE, 1_908, false);
    assert_array_insert_source::<16>(ARRAY_INSERT_SOURCE, 1_916, false);
}

#[test]
fn array_insert_proxy_order_is_stable_for_every_dispatch_batch() {
    assert_array_insert_source::<1>(ARRAY_INSERT_PROXY_SOURCE, 1_921, false);
    assert_array_insert_source::<2>(ARRAY_INSERT_PROXY_SOURCE, 1_922, false);
    assert_array_insert_source::<4>(ARRAY_INSERT_PROXY_SOURCE, 1_924, false);
    assert_array_insert_source::<8>(ARRAY_INSERT_PROXY_SOURCE, 1_928, false);
    assert_array_insert_source::<16>(ARRAY_INSERT_PROXY_SOURCE, 1_936, false);
}

#[test]
fn array_insert_checks_safe_integer_overflow() {
    assert_array_insert_source::<8>(ARRAY_INSERT_OVERFLOW_SOURCE, 1_940, false);
}

#[test]
/// Uses a larger atom quota because generic indexed operations materialize property keys.
fn array_insert_long_sync_move_does_not_grow_the_rust_stack() {
    let module = compile_array_insert_source(ARRAY_INSERT_LONG_SOURCE, 1_944);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(12 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("large-atom insertion isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("long synchronous Array insertion executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long synchronous Array insertion returned {outcome:?}"
    );
}

#[test]
fn array_insert_state_survives_forced_major_collections() {
    assert_array_insert_source::<8>(ARRAY_INSERT_SOURCE, 1_941, true);
    assert_array_insert_source::<8>(ARRAY_INSERT_PROXY_SOURCE, 1_942, true);
    assert_array_insert_source::<8>(ARRAY_INSERT_GC_SOURCE, 1_943, true);
}

/// Compiles and executes one push/unshift fixture under a dispatch and GC policy.
fn assert_array_insert_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_insert_source(source, source_id);
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
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("Array insertion fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one push/unshift fixture independently of isolate policy.
fn compile_array_insert_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-insert-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array insertion fixture compiles")
}
