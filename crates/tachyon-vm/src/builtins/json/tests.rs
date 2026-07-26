use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{ForcedCollectionMode, HeapLimit, SPAN_SIZE_BYTES};

use super::*;

const PRETTY_JSON_SOURCE: &str = r#"
var nested = JSON.stringify({a: [1, {b: 2}]}, null, "xy");
var expected = "{\nxy\"a\": [\nxyxy1,\nxyxy{\nxyxyxy\"b\": 2\nxyxy}\nxy]\n}";
nested === expected &&
JSON.stringify({a: 1}, null, "0123456789ignored") ===
  "{\n0123456789\"a\": 1\n}";
"#;

#[test]
fn primitive_json_indentation_is_stable_for_every_dispatch_batch() {
    assert_pretty_json_batch::<1>(false);
    assert_pretty_json_batch::<2>(false);
    assert_pretty_json_batch::<4>(false);
    assert_pretty_json_batch::<8>(false);
    assert_pretty_json_batch::<16>(false);
}

#[test]
fn primitive_json_indentation_survives_forced_major_collection() {
    assert_pretty_json_batch::<8>(true);
}

#[test]
fn hex_escape_digits_are_ascii_only() {
    assert_eq!(hex_value(u16::from(b'0')), Some(0));
    assert_eq!(hex_value(u16::from(b'f')), Some(15));
    assert_eq!(hex_value(u16::from(b'G')), None);
}

/// Runs nested primitive JSON indentation under one dispatch and collection policy.
fn assert_pretty_json_batch<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(1_900 + N as u32),
                SourceName::new("json-primitive-indentation"),
                MediaType::JavaScript,
                Arc::from(PRETTY_JSON_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("JSON indentation fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(9 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("JSON indentation isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("JSON indentation fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "JSON indentation batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
