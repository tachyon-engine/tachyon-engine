use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_OF_SOURCE: &str = r#"
var trace = "";
function Result(length) {
  trace += "c" + length + ";";
  Object.defineProperty(this, "0", {
    set: function() { throw new Error("Set must not be used"); },
    configurable: true
  });
}
var result = Array.of.call(Result, 11, 22, 33);
var ordinary = Array.of.call(null, 4, 5);
result instanceof Result && result.length === 3 && result[0] === 11 &&
  result[1] === 22 && result[2] === 33 && trace === "c3;" &&
  Array.isArray(ordinary) && ordinary.length === 2 && ordinary[0] === 4 && ordinary[1] === 5;
"#;

const ARRAY_OF_PROXY_SOURCE: &str = r#"
var trace = "";
function Result() {
  return new Proxy({}, {
    defineProperty: function(target, key, descriptor) {
      trace += "d" + key + ";";
      return true;
    },
    set: function(target, key, value, receiver) {
      trace += "s" + key + ";";
      return true;
    }
  });
}
Array.of.call(Result, "a", "b");
trace === "d0;d1;slength;";
"#;

#[test]
fn array_of_is_stable_for_every_dispatch_batch() {
    assert_array_of_source::<1>(ARRAY_OF_SOURCE, 1_871, false);
    assert_array_of_source::<2>(ARRAY_OF_SOURCE, 1_872, false);
    assert_array_of_source::<4>(ARRAY_OF_SOURCE, 1_874, false);
    assert_array_of_source::<8>(ARRAY_OF_SOURCE, 1_878, false);
    assert_array_of_source::<16>(ARRAY_OF_SOURCE, 1_886, false);
}

#[test]
fn array_of_proxy_state_survives_forced_major_collections() {
    assert_array_of_source::<8>(ARRAY_OF_PROXY_SOURCE, 1_887, false);
    assert_array_of_source::<8>(ARRAY_OF_PROXY_SOURCE, 1_888, true);
}

/// Compiles and executes one Array.of fixture under a selected dispatch and GC policy.
fn assert_array_of_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-static-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array.of fixture compiles");
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
        .expect("Array.of fixture executes");
    let completed = match outcome {
        RunOutcome::Completed(value) => value.as_immediate(),
        _ => None,
    };
    assert_eq!(completed, Some(Immediate::True), "outcome={outcome:?}");
}
