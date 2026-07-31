use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;
use crate::tests::fixtures::test_isolate;

const ENUMERATION_SOURCE: &str = r#"
var effects = "";
var source = {
    get a() { effects = effects + "a"; delete this.c; return 1; },
    get b() { effects = effects + "b"; return { value: 2 }; },
    c: 3
};
var values = Object.values(source);
var entries = Object.entries(source);
var chars = Object.values("abc");
effects === "abab" &&
    values.length === 2 && values[0] === 1 && values[1].value === 2 &&
    entries.length === 2 && entries[0][0] === "a" && entries[0][1] === 1 &&
    entries[1][0] === "b" && entries[1][1].value === 2 &&
    chars.length === 3 && chars[0] === "a" && chars[1] === "b" && chars[2] === "c";
"#;

const PROXY_ENUMERATION_SOURCE: &str = r#"
var effects = "";
var code = 0;
var target = { a: 1, b: 2 };
var handler = {
    ownKeys: function(actual) { if (actual !== target) code = code + 1; effects = effects + "o"; return ["b", "a"]; },
    getOwnPropertyDescriptor: function(target, key) {
        effects = effects + "d" + key;
        return Object.getOwnPropertyDescriptor(target, key);
    },
    get: function(actual, key, receiver) {
        if (actual !== target) code = code + 2;
        if (receiver !== source) code = code + 4;
        effects = effects + "g" + key;
        return actual[key];
    }
};
var check = {
    get: function(actual, key) {
        if (!(key in actual)) code = code + 8;
        return actual[key];
    }
};
var source = new Proxy(target, new Proxy(handler, check));
var values = Object.values(source);
code === 0 && effects === "odbgbdaga" &&
    values.length === 2 && values[0] === 2 && values[1] === 1;
"#;

#[test]
fn object_values_and_entries_are_stable_across_dispatch_batches() {
    assert_enumeration_batch::<1>(ENUMERATION_SOURCE, 7_510, ForcedCollectionMode::None);
    assert_enumeration_batch::<2>(ENUMERATION_SOURCE, 7_511, ForcedCollectionMode::None);
    assert_enumeration_batch::<4>(ENUMERATION_SOURCE, 7_512, ForcedCollectionMode::None);
    assert_enumeration_batch::<8>(ENUMERATION_SOURCE, 7_513, ForcedCollectionMode::None);
    assert_enumeration_batch::<16>(ENUMERATION_SOURCE, 7_514, ForcedCollectionMode::None);
}

#[test]
fn object_enumeration_protocol_survives_forced_major_collections() {
    assert_enumeration_batch::<1>(ENUMERATION_SOURCE, 7_515, ForcedCollectionMode::Major);
    assert_enumeration_batch::<16>(PROXY_ENUMERATION_SOURCE, 7_516, ForcedCollectionMode::Major);
}

/// Compiles and executes one observable enumeration fixture for a dispatch/GC configuration.
fn assert_enumeration_batch<const N: usize>(
    source: &str,
    source_id: u32,
    forced: ForcedCollectionMode,
) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("object-enumeration"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Object enumeration fixture compiles");
    let mut isolate = test_isolate();
    isolate.heap.set_forced_collection_mode(forced);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("Object enumeration fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} with {forced:?} returned {outcome:?}; numeric={:?}",
        match outcome {
            RunOutcome::Completed(value) => numeric_value(value),
            RunOutcome::Thrown(_) | RunOutcome::BudgetExhausted => None,
        }
    );
}
