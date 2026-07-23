use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_REVERSE_SOURCE: &str = r#"
var dense = [1, 2, 3, 4];
var returned = dense.reverse();
var denseOk = returned === dense && dense[0] === 4 && dense[1] === 3 &&
  dense[2] === 2 && dense[3] === 1;

var upperOnly = [, 2, 3];
upperOnly.reverse();
var upperOnlyOk = upperOnly[0] === 3 && upperOnly[1] === 2 && !(2 in upperOnly);

var lowerOnly = [1];
lowerOnly.length = 3;
lowerOnly.reverse();
var lowerOnlyOk = !(0 in lowerOnly) && !(1 in lowerOnly) && lowerOnly[2] === 1;

var trace = "";
var target = { 0: "a", 3: "d", length: 4 };
var proxy = new Proxy(target, {
  get: function(object, key, receiver) {
    trace += "g" + key + ";";
    return Reflect.get(object, key, receiver);
  },
  has: function(object, key) {
    trace += "h" + key + ";";
    return key in object;
  },
  set: function(object, key, value) {
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
var proxyReturned = Array.prototype.reverse.call(proxy);
var proxyOk = proxyReturned === proxy && target[0] === "d" && target[3] === "a" &&
  !(1 in target) && !(2 in target) &&
  trace === "glength;h0;g0;h3;g3;s0;s3;h1;h2;";

denseOk && upperOnlyOk && lowerOnlyOk && proxyOk &&
  Array.prototype.reverse.call(true) !== true;
"#;

const ARRAY_REVERSE_GC_SOURCE: &str = r#"
var left = { id: 1 };
var right = { id: 2 };
var trace = "";
var source = {
  get 0() { return left; },
  get 1() { return right; },
  get length() { return { valueOf: function() { return 2; } }; },
  set 0(value) { trace += "l" + value.id; },
  set 1(value) { trace += "r" + value.id; }
};
var returned = Array.prototype.reverse.call(source);
returned === source && left.id === 1 && right.id === 2 && trace === "l2r1";
"#;

const ARRAY_REVERSE_LONG_SOURCE: &str = r#"
var sparse = [];
sparse.length = 20000;
sparse[0] = 1;
sparse[19999] = 2;
var returned = sparse.reverse();
returned === sparse && sparse[0] === 2 && sparse[19999] === 1 &&
  !(1 in sparse) && !(19998 in sparse);
"#;

#[test]
fn array_reverse_is_stable_for_every_dispatch_batch() {
    assert_array_reverse_source::<1>(ARRAY_REVERSE_SOURCE, 1_981, false);
    assert_array_reverse_source::<2>(ARRAY_REVERSE_SOURCE, 1_982, false);
    assert_array_reverse_source::<4>(ARRAY_REVERSE_SOURCE, 1_984, false);
    assert_array_reverse_source::<8>(ARRAY_REVERSE_SOURCE, 1_988, false);
    assert_array_reverse_source::<16>(ARRAY_REVERSE_SOURCE, 1_996, false);
}

#[test]
fn array_reverse_state_survives_forced_major_collections() {
    assert_array_reverse_source::<8>(ARRAY_REVERSE_SOURCE, 2_000, true);
    assert_array_reverse_source::<8>(ARRAY_REVERSE_GC_SOURCE, 2_001, true);
}

#[test]
/// Uses a larger atom quota because generic indexed operations materialize property keys.
fn array_reverse_long_sync_scan_does_not_grow_the_rust_stack() {
    let module = compile_array_reverse_source(ARRAY_REVERSE_LONG_SOURCE, 2_002);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(12 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("large-atom reverse isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("long synchronous reverse executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long synchronous reverse returned {outcome:?}"
    );
}

/// Compiles and executes one reverse fixture under a dispatch and GC policy.
fn assert_array_reverse_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_reverse_source(source, source_id);
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
        .expect("reverse fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one reverse fixture independently of isolate policy.
fn compile_array_reverse_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-reverse-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("reverse fixture compiles")
}
