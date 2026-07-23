use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_COPY_SOURCE: &str = r#"
var order = "";
var source = { length: 3 };
Object.defineProperty(source, "0", { get: function() { order += "0"; return "a"; } });
Object.defineProperty(source, "1", { get: function() { order += "1"; return "b"; } });
Object.defineProperty(source, "2", { get: function() { order += "2"; return "c"; } });
var reversed = Array.prototype.toReversed.call(source);
var descending = order === "210" && reversed[0] === "c" && reversed[2] === "a";

var sparse = [, 2, , 4];
sparse.constructor = { get [Symbol.species]() { throw new Error("species"); } };
var dense = sparse.toReversed();
var denseHoles = dense.length === 4 && ("0" in dense) && ("1" in dense) &&
  ("2" in dense) && ("3" in dense) && dense[0] === 4 && dense[1] === undefined;

order = "";
var replaced = Array.prototype.with.call(source, 1, "x");
var replacementSkippedGet = order === "02" && replaced[0] === "a" &&
  replaced[1] === "x" && replaced[2] === "c";
var negative = [1, 2, 3].with(-1, 9);
var range = false;
try { negative.with(3, 0); } catch (error) { range = error instanceof RangeError; }
var converted = false;
var hugeOrder = false;
try {
  Array.prototype.with.call(
    { length: 4294967296 },
    { valueOf: function() { converted = true; return 0; } },
    1
  );
} catch (error) { hugeOrder = converted && error instanceof RangeError; }

descending + 2 * denseHoles + 4 * replacementSkippedGet +
  8 * (negative[0] === 1 && negative[1] === 2 && negative[2] === 9) +
  16 * range + 32 * hugeOrder;
"#;

#[test]
fn array_copy_methods_are_stable_for_every_dispatch_batch() {
    assert_array_copy_source::<1>(ARRAY_COPY_SOURCE, 1_921, false);
    assert_array_copy_source::<2>(ARRAY_COPY_SOURCE, 1_922, false);
    assert_array_copy_source::<4>(ARRAY_COPY_SOURCE, 1_924, false);
    assert_array_copy_source::<8>(ARRAY_COPY_SOURCE, 1_928, false);
    assert_array_copy_source::<16>(ARRAY_COPY_SOURCE, 1_936, false);
}

#[test]
fn array_copy_state_survives_forced_major_collections() {
    assert_array_copy_source::<8>(ARRAY_COPY_SOURCE, 1_938, true);
}

/// Compiles and executes one copy-method fixture under a selected dispatch and GC policy.
fn assert_array_copy_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-copy.js"),
                MediaType::JavaScript,
                Arc::<str>::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("array copy fixture must compile");
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
                fuel: 250_000,
                quantum: 250_000,
            },
        )
        .expect("array copy fixture must execute");
    assert_eq!(outcome, RunOutcome::Completed(Value::from_i32(63)));
}
