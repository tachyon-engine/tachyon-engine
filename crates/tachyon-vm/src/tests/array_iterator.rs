use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_ITERATOR_CREATORS_SOURCE: &str = r#"
var trace = "";
var source = {
  get length() { trace += "l"; return 2; },
  get 0() { trace += "a"; return 7; },
  1: 8
};
var keys = Array.prototype.keys.call(source);
var key0 = keys.next();
var key1 = keys.next();
var keyDone = keys.next();
var entries = Array.prototype.entries.call(source);
var entry0 = entries.next();
var entry1 = entries.next();
var entryDone = entries.next();
key0.value === 0 && key0.done === false &&
  key1.value === 1 && key1.done === false && keyDone.done === true &&
  entry0.value[0] === 0 && entry0.value[1] === 7 && entry0.done === false &&
  entry1.value[0] === 1 && entry1.value[1] === 8 && entry1.done === false &&
  entryDone.done === true && trace === "llllall";
"#;

#[test]
fn array_iterator_creators_are_stable_for_every_dispatch_batch() {
    assert_array_iterator_source::<1>(2_201, false);
    assert_array_iterator_source::<2>(2_202, false);
    assert_array_iterator_source::<4>(2_204, false);
    assert_array_iterator_source::<8>(2_208, false);
    assert_array_iterator_source::<16>(2_216, false);
}

#[test]
fn array_iterator_entry_projection_survives_forced_major_collection() {
    assert_array_iterator_source::<8>(2_220, true);
}

/// Compiles and executes the creator fixture under one dispatch and GC policy.
fn assert_array_iterator_source<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_array_iterator_source(source_id);
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
        .expect("Array iterator creator fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles the shared creator fixture without selecting a collection policy.
fn compile_array_iterator_source(source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-iterator-creator-fixture"),
                MediaType::JavaScript,
                Arc::from(ARRAY_ITERATOR_CREATORS_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Array iterator creator fixture compiles")
}
