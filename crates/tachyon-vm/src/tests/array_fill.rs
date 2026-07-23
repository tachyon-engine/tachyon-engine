use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_FILL_SOURCE: &str = r#"
var dense = [0, , 2, 3];
var returned = dense.fill(7, 1, -1);
var denseOk = returned === dense && dense.length === 4 && dense[0] === 0 &&
  dense[1] === 7 && dense[2] === 7 && dense[3] === 3;

var trace = "";
var target = {
  get length() {
    trace += "g";
    return { valueOf: function() { trace += "l"; return 4; } };
  }
};
var proxy = new Proxy(target, {
  set: function(object, key, value) {
    trace += "p" + key + value + ";";
    object[key] = value;
    return true;
  }
});
var proxyReturned = Array.prototype.fill.call(
  proxy,
  5,
  { valueOf: function() { trace += "s"; return 1; } },
  { valueOf: function() { trace += "e"; return 3; } }
);
var proxyOk = proxyReturned === proxy && target[1] === 5 && target[2] === 5 &&
  trace === "glsep15;p25;";

denseOk && proxyOk && Array.prototype.fill.call(true, 1) !== true;
"#;

const ARRAY_FILL_GC_SOURCE: &str = r#"
var retained = { id: 9 };
var trace = "";
var source = {
  get length() { return { valueOf: function() { return 2; } }; },
  set 0(value) { trace += "a" + value.id; },
  set 1(value) { trace += "b" + value.id; }
};
var returned = Array.prototype.fill.call(source, retained);
returned === source && retained.id === 9 && trace === "a9b9";
"#;

const ARRAY_FILL_LONG_SOURCE: &str = r#"
var sparse = [];
sparse.length = 20000;
var returned = sparse.fill(6);
returned === sparse && sparse.length === 20000 && sparse[0] === 6 &&
  sparse[9999] === 6 && sparse[19999] === 6;
"#;

#[test]
fn array_fill_is_stable_for_every_dispatch_batch() {
    assert_array_fill_source::<1>(ARRAY_FILL_SOURCE, 2_011, false);
    assert_array_fill_source::<2>(ARRAY_FILL_SOURCE, 2_012, false);
    assert_array_fill_source::<4>(ARRAY_FILL_SOURCE, 2_014, false);
    assert_array_fill_source::<8>(ARRAY_FILL_SOURCE, 2_018, false);
    assert_array_fill_source::<16>(ARRAY_FILL_SOURCE, 2_026, false);
}

#[test]
fn array_fill_state_survives_forced_major_collections() {
    assert_array_fill_source::<8>(ARRAY_FILL_SOURCE, 2_030, true);
    assert_array_fill_source::<8>(ARRAY_FILL_GC_SOURCE, 2_031, true);
}

#[test]
/// Uses a larger atom quota because generic indexed operations materialize property keys.
fn array_fill_long_sync_scan_does_not_grow_the_rust_stack() {
    let module = compile_array_fill_source(ARRAY_FILL_LONG_SOURCE, 2_032);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(32 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 32_768).with_max_shapes(32_768),
    ))
    .expect("large-atom fill isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("long synchronous fill executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long synchronous fill returned {outcome:?}"
    );
}

/// Compiles and executes one fill fixture under a dispatch and GC policy.
fn assert_array_fill_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_fill_source(source, source_id);
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
        .expect("fill fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one fill fixture independently of isolate policy.
fn compile_array_fill_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-fill-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("fill fixture compiles")
}
