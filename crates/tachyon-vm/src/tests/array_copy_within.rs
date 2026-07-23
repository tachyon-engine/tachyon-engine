use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const COPY_WITHIN_SOURCE: &str = r#"
var dense = [1, 2, 3, 4, 5];
var returned = dense.copyWithin(1, 3);
var denseOk = returned === dense && dense.length === 5 &&
  dense[0] === 1 && dense[1] === 4 && dense[2] === 5 && dense[3] === 4;

var sparse = [, 7, , 9];
sparse.copyWithin(1, 0, 3);
var sparseOk = !(1 in sparse) && sparse[2] === 7 && !(3 in sparse);

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
var proxyReturned = Array.prototype.copyWithin.call(proxy, 1, 0, 3);
var proxyOk = proxyReturned === proxy && target[1] === "a" && !(2 in target) &&
  trace === "glength;h1;d2;h0;g0;s1;";

var conversionTrace = "";
var converted = [0, 1, 2];
converted.copyWithin(
  { valueOf: function() { conversionTrace += "t"; return 1; } },
  { valueOf: function() { conversionTrace += "s"; return 0; } },
  { valueOf: function() { conversionTrace += "e"; return 1; } }
);

denseOk && sparseOk && proxyOk && conversionTrace === "tse" &&
  converted[0] === 0 && converted[1] === 0 && converted[2] === 2 &&
  Array.prototype.copyWithin.call(true, 0, 0) !== true;
"#;

const COPY_WITHIN_GC_SOURCE: &str = r#"
var retained = { id: 7 };
var observed = 0;
var source = {
  get 0() { return retained; },
  get length() { return { valueOf: function() { return 2; } }; },
  set 1(value) { observed = value.id; }
};
var returned = Array.prototype.copyWithin.call(source, 1, 0, 1);
returned === source && retained.id === 7 && observed === 7;
"#;

const COPY_WITHIN_LONG_SOURCE: &str = r#"
var sparse = [];
sparse.length = 20000;
sparse[19999] = 7;
var returned = sparse.copyWithin(0, 1);
returned === sparse && sparse[19998] === 7 && sparse[19999] === 7;
"#;

#[test]
fn copy_within_is_stable_for_every_dispatch_batch() {
    assert_copy_within_source::<1>(COPY_WITHIN_SOURCE, 1_951, false);
    assert_copy_within_source::<2>(COPY_WITHIN_SOURCE, 1_952, false);
    assert_copy_within_source::<4>(COPY_WITHIN_SOURCE, 1_954, false);
    assert_copy_within_source::<8>(COPY_WITHIN_SOURCE, 1_958, false);
    assert_copy_within_source::<16>(COPY_WITHIN_SOURCE, 1_966, false);
}

#[test]
fn copy_within_state_survives_forced_major_collections() {
    assert_copy_within_source::<8>(COPY_WITHIN_SOURCE, 1_970, true);
    assert_copy_within_source::<8>(COPY_WITHIN_GC_SOURCE, 1_971, true);
}

#[test]
/// Uses a larger atom quota because generic indexed operations materialize property keys.
fn copy_within_long_sync_move_does_not_grow_the_rust_stack() {
    let module = compile_copy_within_source(COPY_WITHIN_LONG_SOURCE, 1_972);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(12 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("large-atom copyWithin isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("long synchronous copyWithin executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long synchronous copyWithin returned {outcome:?}"
    );
}

/// Compiles and executes one copyWithin fixture under a dispatch and GC policy.
fn assert_copy_within_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_copy_within_source(source, source_id);
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
        .expect("copyWithin fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one copyWithin fixture independently of isolate policy.
fn compile_copy_within_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-copy-within-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("copyWithin fixture compiles")
}
