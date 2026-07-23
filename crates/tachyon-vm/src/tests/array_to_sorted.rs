use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

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

/// Compiles and executes one stable-sort fixture under a selected dispatch and GC policy.
fn assert_array_to_sorted_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-to-sorted-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array.toSorted fixture compiles");
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
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .expect("Array.toSorted fixture executes");
    assert_eq!(
        outcome,
        RunOutcome::Completed(Value::from_immediate(Immediate::True))
    );
}
