use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_FLAT_MAP_SOURCE: &str = r#"
var source = [1, , 3];
var thisArg = { offset: 10 };
var result = source.flatMap(function(value, index, receiver) {
  if (receiver !== source) throw "receiver";
  return [value + this.offset, index];
}, thisArg);
result.length === 4 && result[0] === 11 && result[1] === 0 &&
  result[2] === 13 && result[3] === 2;
"#;

const ARRAY_FLAT_MAP_PROXY_SOURCE: &str = r#"
var trace = "";
var source = new Proxy([2, , 3], {
  get: function(target, key, receiver) {
    trace += "g" + key + ";";
    return Reflect.get(target, key, receiver);
  },
  has: function(target, key) {
    trace += "h" + key + ";";
    return Reflect.has(target, key);
  }
});
var result = source.flatMap(function(value) { return [value]; });
result.length === 2 && result[0] === 2 && result[1] === 3 &&
  trace === "gflatMap;glength;gconstructor;h0;g0;h1;h2;g2;";
"#;

const ARRAY_FLAT_MAP_SPECIES_SOURCE: &str = r#"
var trace = "";
var target = {};
var proxy;
function Species(length) {
  trace += "n" + length + ";";
  proxy = new Proxy(target, {
    defineProperty: function(object, key, descriptor) {
      trace += "d" + key + ";";
      return Reflect.defineProperty(object, key, descriptor);
    }
  });
  return proxy;
}
var source = [1, , 2];
source.constructor = { [Symbol.species]: Species };
var result = source.flatMap(function(value) { return [value, value + 10]; });
result === proxy && target[0] === 1 && target[1] === 11 &&
  target[2] === 2 && target[3] === 12 && target.length === undefined &&
  trace === "n0;d0;d1;d2;d3;";
"#;

const ARRAY_FLAT_MAP_BOUND_SOURCE: &str = r#"
var result = [0, 0].flatMap(function() { return this; }.bind([1, 2]));
result.length === 4 && result[0] === 1 && result[1] === 2 &&
  result[2] === 1 && result[3] === 2;
"#;

const ARRAY_FLAT_MAP_SPARSE_SOURCE: &str = r#"
var source = { length: 10001 };
source[10000] = 7;
var result = Array.prototype.flatMap.call(source, function(value) { return [value, value * 2]; });
result.length === 2 && result[0] === 7 && result[1] === 14;
"#;

#[test]
fn array_flat_map_is_stable_for_every_dispatch_batch() {
    assert_array_flat_map_source::<1>(ARRAY_FLAT_MAP_SOURCE, 2_001, false);
    assert_array_flat_map_source::<2>(ARRAY_FLAT_MAP_SOURCE, 2_002, false);
    assert_array_flat_map_source::<4>(ARRAY_FLAT_MAP_SOURCE, 2_004, false);
    assert_array_flat_map_source::<8>(ARRAY_FLAT_MAP_SOURCE, 2_008, false);
    assert_array_flat_map_source::<16>(ARRAY_FLAT_MAP_SOURCE, 2_016, false);
}

#[test]
fn array_flat_map_proxy_order_is_stable_for_every_dispatch_batch() {
    assert_array_flat_map_source::<1>(ARRAY_FLAT_MAP_PROXY_SOURCE, 2_021, false);
    assert_array_flat_map_source::<2>(ARRAY_FLAT_MAP_PROXY_SOURCE, 2_022, false);
    assert_array_flat_map_source::<4>(ARRAY_FLAT_MAP_PROXY_SOURCE, 2_024, false);
    assert_array_flat_map_source::<8>(ARRAY_FLAT_MAP_PROXY_SOURCE, 2_028, false);
    assert_array_flat_map_source::<16>(ARRAY_FLAT_MAP_PROXY_SOURCE, 2_036, false);
}

#[test]
fn array_flat_map_species_and_bound_prefix_survive_forced_major() {
    assert_array_flat_map_source::<8>(ARRAY_FLAT_MAP_SPECIES_SOURCE, 2_040, true);
    assert_array_flat_map_source::<8>(ARRAY_FLAT_MAP_BOUND_SOURCE, 2_041, true);
}

#[test]
fn array_flat_map_sparse_scan_does_not_grow_atoms_or_the_rust_stack() {
    assert_array_flat_map_source::<8>(ARRAY_FLAT_MAP_SPARSE_SOURCE, 2_042, false);
}

/// Compiles and executes one flatMap fixture under a selected dispatch and GC policy.
fn assert_array_flat_map_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_flat_map_source(source, source_id);
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
        .expect("Array flatMap fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one flatMap fixture without coupling it to an isolate collection policy.
fn compile_array_flat_map_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-flat-map-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array flatMap fixture compiles")
}
