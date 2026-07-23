use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_REMOVE_SOURCE: &str = r#"
var values = [1, 2, 3];
var popped = values.pop();
var shifted = values.shift();
var denseOk = popped === 3 && shifted === 1 && values.length === 1 && values[0] === 2;

var sparse = [, 4, , 8];
var sparseFirst = sparse.shift();
var sparseOk = sparseFirst === undefined && sparse.length === 3 && sparse[0] === 4 &&
  !(1 in sparse) && sparse[2] === 8 && !(3 in sparse);

var trace = "";
var empty = {
  get length() { trace += "g"; return 0; },
  set length(value) { trace += "s" + value; }
};
var emptyResult = Array.prototype.pop.call(empty);
var zeroOk = emptyResult === undefined && trace === "gs0";

denseOk && sparseOk && zeroOk;
"#;

const ARRAY_REMOVE_PROXY_SOURCE: &str = r#"
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
var first = Array.prototype.shift.call(proxy);
var shiftTrace = trace;
trace = "";
var last = Array.prototype.pop.call(proxy);
first === "a" && last === "c" && target.length === 1 && !(0 in target) &&
  shiftTrace === "glength;g0;h1;d0;h2;g2;s1;d2;slength;" &&
  trace === "glength;g1;d1;slength;";
"#;

const ARRAY_REMOVE_GC_SOURCE: &str = r#"
var marker = { alive: 41 };
var trace = "";
var source = {
  get length() { return { valueOf: function() { return 2; } }; },
  get 0() { return marker; },
  get 1() { return 9; },
  set 0(value) { trace += "m" + value; },
  set length(value) { trace += "l" + value; }
};
var result = Array.prototype.shift.call(source);
result === marker && result.alive === 41 && trace === "m9l1";
"#;

#[test]
fn array_remove_is_stable_for_every_dispatch_batch() {
    assert_array_remove_source::<1>(ARRAY_REMOVE_SOURCE, 1_801, false);
    assert_array_remove_source::<2>(ARRAY_REMOVE_SOURCE, 1_802, false);
    assert_array_remove_source::<4>(ARRAY_REMOVE_SOURCE, 1_804, false);
    assert_array_remove_source::<8>(ARRAY_REMOVE_SOURCE, 1_808, false);
    assert_array_remove_source::<16>(ARRAY_REMOVE_SOURCE, 1_816, false);
}

#[test]
fn array_remove_proxy_order_is_stable_for_every_dispatch_batch() {
    assert_array_remove_source::<1>(ARRAY_REMOVE_PROXY_SOURCE, 1_821, false);
    assert_array_remove_source::<2>(ARRAY_REMOVE_PROXY_SOURCE, 1_822, false);
    assert_array_remove_source::<4>(ARRAY_REMOVE_PROXY_SOURCE, 1_824, false);
    assert_array_remove_source::<8>(ARRAY_REMOVE_PROXY_SOURCE, 1_828, false);
    assert_array_remove_source::<16>(ARRAY_REMOVE_PROXY_SOURCE, 1_836, false);
}

#[test]
fn array_remove_state_survives_forced_major_collections() {
    assert_array_remove_source::<8>(ARRAY_REMOVE_SOURCE, 1_840, true);
    assert_array_remove_source::<8>(ARRAY_REMOVE_PROXY_SOURCE, 1_841, true);
    assert_array_remove_source::<8>(ARRAY_REMOVE_GC_SOURCE, 1_842, true);
}

/// Compiles and executes one pop/shift fixture under a selected dispatch and GC policy.
fn assert_array_remove_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_remove_source(source, source_id);
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
        .expect("Array removal fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one pop/shift fixture without coupling it to collection policy.
fn compile_array_remove_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-remove-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array removal fixture compiles")
}
