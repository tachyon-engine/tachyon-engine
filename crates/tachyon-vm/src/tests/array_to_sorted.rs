use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

const ARRAY_TO_SORTED_SOURCE: &str = r#"
var trace = "";
var source = { length: 3 };
Object.defineProperty(source, "0", { get: function() { trace += "g0"; return 3; } });
Object.defineProperty(source, "1", { get: function() { trace += "g1"; return 1; } });
Object.defineProperty(source, "2", { get: function() { trace += "g2"; return 2; } });
var sorted = Array.prototype.toSorted.call(source, function(a, b) {
  trace += "c";
  return { valueOf: function() { trace += "n"; return a - b; } };
});
var collectedFirst = trace.slice(0, 6) === "g0g1g2" && sorted[0] === 1 && sorted[2] === 3;

var lexical = [333, 33, 3, 222, 22, 2, 111, 11, 1].toSorted();
var defaultOrder = lexical.join(",") === "1,11,111,2,22,222,3,33,333";

var stable = [
  { key: 1, id: "a" }, { key: 0, id: "b" },
  { key: 1, id: "c" }, { key: 0, id: "d" }
].toSorted(function(a, b) { return a.key - b.key; });
var stableOrder = stable[0].id === "b" && stable[1].id === "d" &&
  stable[2].id === "a" && stable[3].id === "c";

var calls = 0;
var dense = [, 2, undefined, 1].toSorted(function(a, b) { calls++; return a - b; });
var undefinedOrder = calls > 0 && dense.length === 4 &&
  dense[0] === 1 && dense[1] === 2 && dense[2] === undefined && dense[3] === undefined &&
  ("2" in dense) && ("3" in dense);

var stringTrace = "";
function StringValue(value, name) {
  this.toString = function() { stringTrace += name; return value; };
}
var objects = [new StringValue("b", "b"), new StringValue("a", "a")].toSorted();
var stringOrder = objects[0].toString() === "a" && objects[1].toString() === "b" &&
  stringTrace.slice(0, 2) === "ba";

collectedFirst && defaultOrder && stableOrder && undefinedOrder && stringOrder;
"#;

const ARRAY_SORT_SOURCE: &str = r#"
var stable = [];
for (var i = 128; i >= 0; i--) {
  stable.push({ key: i % 5, id: i });
}
var compareCalls = 0;
stable.sort(function(a, b) {
  compareCalls++;
  return { valueOf: function() { return a.key - b.key; } };
});
var stableOk = compareCalls > 0 && stable.length === 129;
for (var j = 1; j < stable.length; j++) {
  if (stable[j - 1].key > stable[j].key ||
      (stable[j - 1].key === stable[j].key && stable[j - 1].id < stable[j].id)) {
    stableOk = false;
  }
}

var sparse = { length: 1000 };
sparse[0] = 3;
sparse[500] = undefined;
sparse[999] = 1;
var sawUndefined = false;
Array.prototype.sort.call(sparse, function(a, b) {
  if (a === undefined || b === undefined) sawUndefined = true;
  return a - b;
});
var sparseOk = sparse[0] === 1 && sparse[1] === 3 && sparse[2] === undefined &&
  !("3" in sparse) && !("999" in sparse) && !sawUndefined;

stableOk && sparseOk;
"#;

const ARRAY_SORT_LONG_SYNC_SOURCE: &str = r#"
var values = [];
for (var i = 2047; i >= 0; i--) values.push(i);
values.sort();
values.length === 2048 && values[0] === 0 && values[1] === 1 &&
  values[values.length - 1] === 999;
"#;

#[test]
fn array_to_sorted_is_stable_for_every_dispatch_batch() {
    assert_array_to_sorted_source::<1>(ARRAY_TO_SORTED_SOURCE, 1_951, false);
    assert_array_to_sorted_source::<2>(ARRAY_TO_SORTED_SOURCE, 1_952, false);
    assert_array_to_sorted_source::<4>(ARRAY_TO_SORTED_SOURCE, 1_954, false);
    assert_array_to_sorted_source::<8>(ARRAY_TO_SORTED_SOURCE, 1_958, false);
    assert_array_to_sorted_source::<16>(ARRAY_TO_SORTED_SOURCE, 1_966, false);
}

#[test]
fn array_to_sorted_state_survives_forced_major_collections() {
    assert_array_to_sorted_source::<8>(ARRAY_TO_SORTED_SOURCE, 1_968, true);
}

#[test]
fn array_sort_is_stable_for_every_dispatch_batch() {
    assert_array_to_sorted_source::<1>(ARRAY_SORT_SOURCE, 1_971, false);
    assert_array_to_sorted_source::<2>(ARRAY_SORT_SOURCE, 1_972, false);
    assert_array_to_sorted_source::<4>(ARRAY_SORT_SOURCE, 1_974, false);
    assert_array_to_sorted_source::<8>(ARRAY_SORT_SOURCE, 1_978, false);
    assert_array_to_sorted_source::<16>(ARRAY_SORT_SOURCE, 1_986, false);
}

#[test]
fn array_sort_state_survives_forced_major_collections() {
    assert_array_to_sorted_source::<8>(ARRAY_SORT_SOURCE, 1_988, true);
}

#[test]
fn array_sort_long_synchronous_path_does_not_grow_the_rust_stack() {
    assert_array_to_sorted_source::<8>(ARRAY_SORT_LONG_SYNC_SOURCE, 1_989, false);
}

/// Compiles and executes one stable-sort fixture under a selected dispatch and GC policy.
fn assert_array_to_sorted_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("stable-array-sort-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("stable Array sort fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(8_192, 8 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(64 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 8_192),
        RealmLimits::new(64, 8_192).with_max_shapes(8_192),
    ))
    .expect("stable sort test isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 2_097_152,
                quantum: 2_097_152,
            },
        )
        .expect("stable Array sort fixture executes");
    assert_eq!(
        outcome,
        RunOutcome::Completed(Value::from_immediate(Immediate::True))
    );
}
