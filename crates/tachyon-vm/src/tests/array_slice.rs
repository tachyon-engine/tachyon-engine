use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_SLICE_SOURCE: &str = r#"
var trace = "";
var source = [0, , 2, 3];
var start = { valueOf: function() { trace += "s"; return 1; } };
var end = { valueOf: function() { trace += "e"; return 3; } };
var result = source.slice(start, end);
var sparseOk = trace === "se" && result.length === 2 &&
  !(0 in result) && result[1] === 2;
var stringResult = Array.prototype.slice.call("abcd", 1, -1);
var stringOk = stringResult.length === 2 && stringResult[0] === "b" &&
  stringResult[1] === "c";
sparseOk && stringOk;
"#;

const ARRAY_SLICE_SPECIES_SOURCE: &str = r#"
var trace = "";
var target = {};
var resultProxy;
function Species(length) {
  trace += "n" + length + ";";
  resultProxy = new Proxy(target, {
    defineProperty: function(object, key, descriptor) {
      trace += "d" + key + ";";
      return Reflect.defineProperty(object, key, descriptor);
    },
    set: function(object, key, value, receiver) {
      trace += "s" + key + ";";
      object[key] = value;
      return true;
    }
  });
  return resultProxy;
}
var source = [4, , 6];
Object.defineProperty(source, "constructor", {
  get: function() {
    trace += "c;";
    return {
      get [Symbol.species]() {
        trace += "p;";
        return Species;
      }
    };
  }
});
var result = source.slice(0, 3);
result === resultProxy && target[0] === 4 && !(1 in target) && target[2] === 6 &&
  target.length === 3 && trace === "c;p;n3;d0;d2;slength;";
"#;

const ARRAY_SLICE_LONG_SOURCE: &str = r#"
var source = { length: 512 };
source[511] = 9;
var result = Array.prototype.slice.call(source, 0);
result.length === 512 && !(0 in result) && result[511] === 9;
"#;

#[test]
fn array_slice_is_stable_for_every_dispatch_batch() {
    assert_array_slice_source::<1>(ARRAY_SLICE_SOURCE, 1_901, false);
    assert_array_slice_source::<2>(ARRAY_SLICE_SOURCE, 1_902, false);
    assert_array_slice_source::<4>(ARRAY_SLICE_SOURCE, 1_904, false);
    assert_array_slice_source::<8>(ARRAY_SLICE_SOURCE, 1_908, false);
    assert_array_slice_source::<16>(ARRAY_SLICE_SOURCE, 1_916, false);
}

#[test]
fn array_slice_species_proxy_order_is_stable_for_every_dispatch_batch() {
    assert_array_slice_source::<1>(ARRAY_SLICE_SPECIES_SOURCE, 1_921, false);
    assert_array_slice_source::<2>(ARRAY_SLICE_SPECIES_SOURCE, 1_922, false);
    assert_array_slice_source::<4>(ARRAY_SLICE_SPECIES_SOURCE, 1_924, false);
    assert_array_slice_source::<8>(ARRAY_SLICE_SPECIES_SOURCE, 1_928, false);
    assert_array_slice_source::<16>(ARRAY_SLICE_SPECIES_SOURCE, 1_936, false);
}

#[test]
fn array_slice_state_survives_forced_major_collections() {
    assert_array_slice_source::<8>(ARRAY_SLICE_SOURCE, 1_940, true);
    assert_array_slice_source::<8>(ARRAY_SLICE_SPECIES_SOURCE, 1_941, true);
}

#[test]
fn array_slice_long_synchronous_scan_does_not_grow_the_rust_stack() {
    assert_array_slice_source::<8>(ARRAY_SLICE_LONG_SOURCE, 1_942, false);
}

/// Compiles and executes one slice fixture under a selected dispatch and GC policy.
fn assert_array_slice_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_slice_source(source, source_id);
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
        .expect("Array slice fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one slice fixture without coupling it to an isolate collection policy.
fn compile_array_slice_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-slice-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array slice fixture compiles")
}
