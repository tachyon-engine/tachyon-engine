use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const WEAK_COLLECTION_SOURCE: &str = r#"
var map = new WeakMap();
var set = new WeakSet();
var first = {};
var second = {};
map.set(first, 1).set(second, 2);
map.set(first, 3);
set.add(first).add(second);
var beforeDelete = map.get(first) === 3 && map.has(second) && set.has(first);
var deleted = map.delete(first) && set.delete(first);
var afterDelete = !map.has(first) && map.get(first) === undefined && !set.has(first);
map.set(first, 4);
set.add(first);
beforeDelete && deleted && afterDelete && map.get(first) === 4 && set.has(first);
"#;

#[test]
fn weak_collection_hash_semantics_work_for_every_dispatch_batch() {
    assert_weak_collection_source::<1>(false);
    assert_weak_collection_source::<2>(false);
    assert_weak_collection_source::<4>(false);
    assert_weak_collection_source::<8>(false);
    assert_weak_collection_source::<16>(false);
}

#[test]
fn weak_collection_hash_semantics_survive_forced_major_collection() {
    assert_weak_collection_source::<8>(true);
}

#[test]
fn deep_weak_map_chain_survives_explicit_major_collection() {
    let source = r#"
var map = new WeakMap();
var head = {};
var key = head;
for (var i = 0; i < 1000; i++) {
  var next = {};
  map.set(key, next);
  key = next;
}
var traversed = 0;
for (key = head; key !== undefined; key = map.get(key)) traversed++;
traversed === 1001;
"#;
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_451),
                SourceName::new("deep-weak-collection"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("deep WeakMap fixture compiles");
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute(
            &module,
            ExecutionBudget {
                fuel: 2_000_000,
                quantum: u32::MAX,
            },
        )
        .expect("deep WeakMap fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

/// Executes overwrite, deletion, tombstone reuse, and lookup under one dispatch/GC policy.
fn assert_weak_collection_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_450),
                SourceName::new("weak-collection-hash"),
                MediaType::JavaScript,
                Arc::from(WEAK_COLLECTION_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Weak collection fixture compiles");
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
        .expect("Weak collection fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
