use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_FLAT_SOURCE: &str = r#"
var source = [1, [2, , [3]], 4];
var result = source.flat(2);
result.length === 4 && result[0] === 1 && result[1] === 2 &&
  result[2] === 3 && result[3] === 4;
"#;

const ARRAY_FLAT_PROXY_SOURCE: &str = r#"
var trace = "";
var source = new Proxy([1, [2, 3]], {
  get: function(target, key, receiver) {
    trace += "g" + key + ";";
    return Reflect.get(target, key, receiver);
  },
  has: function(target, key) {
    trace += "h" + key + ";";
    return Reflect.has(target, key);
  }
});
var result = source.flat();
result.length === 3 && result[0] === 1 && result[1] === 2 && result[2] === 3 &&
  trace === "gflat;glength;gconstructor;h0;g0;h1;g1;";
"#;

const ARRAY_FLAT_DEEP_SOURCE: &str = r#"
var nested = [42];
for (var index = 0; index < 12; index += 1) nested = [nested];
var result = nested.flat(Infinity);
result.length === 1 && result[0] === 42;
"#;

#[test]
fn array_flat_is_stable_for_every_dispatch_batch() {
    assert_array_flat_source::<1>(ARRAY_FLAT_SOURCE, 2_101, false);
    assert_array_flat_source::<2>(ARRAY_FLAT_SOURCE, 2_102, false);
    assert_array_flat_source::<4>(ARRAY_FLAT_SOURCE, 2_104, false);
    assert_array_flat_source::<8>(ARRAY_FLAT_SOURCE, 2_108, false);
    assert_array_flat_source::<16>(ARRAY_FLAT_SOURCE, 2_116, false);
}

#[test]
fn array_flat_proxy_order_is_stable_for_every_dispatch_batch() {
    assert_array_flat_source::<1>(ARRAY_FLAT_PROXY_SOURCE, 2_121, false);
    assert_array_flat_source::<2>(ARRAY_FLAT_PROXY_SOURCE, 2_122, false);
    assert_array_flat_source::<4>(ARRAY_FLAT_PROXY_SOURCE, 2_124, false);
    assert_array_flat_source::<8>(ARRAY_FLAT_PROXY_SOURCE, 2_128, false);
    assert_array_flat_source::<16>(ARRAY_FLAT_PROXY_SOURCE, 2_136, false);
}

#[test]
fn array_flat_frame_replacement_survives_forced_major_collection() {
    assert_array_flat_source::<8>(ARRAY_FLAT_DEEP_SOURCE, 2_140, true);
}

/// Compiles and executes one flat fixture under a selected dispatch and GC policy.
fn assert_array_flat_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_flat_source(source, source_id);
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
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("Array flat fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one flat fixture without coupling it to an isolate collection policy.
fn compile_array_flat_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-flat-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array flat fixture compiles")
}
