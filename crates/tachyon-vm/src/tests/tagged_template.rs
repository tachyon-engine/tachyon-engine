use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TEMPLATE_SOURCE: &str = r#"
var first;
function tag(parts, value) {
  if (first === undefined) first = parts;
  return parts;
}
function site(value) { return tag`head${value}tail`; }
var one = site(1);
var two = site(2);
var other = tag`head${3}tail`;
var cooked = Object.getOwnPropertyDescriptor(one, "0");
var raw = Object.getOwnPropertyDescriptor(one, "raw");
one === two && one !== other && one[0] === "head" && one[1] === "tail" &&
  one.raw[0] === "head" && one.raw[1] === "tail" &&
  Object.isFrozen(one) && Object.isFrozen(one.raw) &&
  cooked.writable === false && cooked.enumerable === true &&
  cooked.configurable === false && raw.writable === false &&
  raw.enumerable === false && raw.configurable === false;
"#;

const INVALID_ESCAPE_SOURCE: &str = r#"
var captured;
function tag(parts) { captured = parts; }
tag`\unicode`;
captured.hasOwnProperty("0") && captured[0] === undefined &&
  captured.raw[0] === "\\unicode" && Object.isFrozen(captured) &&
  Object.isFrozen(captured.raw);
"#;

#[test]
fn tagged_template_cache_is_stable_for_every_dispatch_batch() {
    assert_template_source::<1>(TEMPLATE_SOURCE, 2_501, false);
    assert_template_source::<2>(TEMPLATE_SOURCE, 2_502, false);
    assert_template_source::<4>(TEMPLATE_SOURCE, 2_504, false);
    assert_template_source::<8>(TEMPLATE_SOURCE, 2_508, false);
    assert_template_source::<16>(TEMPLATE_SOURCE, 2_516, false);
}

#[test]
fn tagged_template_invalid_escape_and_cache_survive_forced_major() {
    assert_template_source::<8>(TEMPLATE_SOURCE, 2_520, true);
    assert_template_source::<8>(INVALID_ESCAPE_SOURCE, 2_521, true);
}

/// Compiles and executes one template fixture under the selected dispatch and GC policy.
fn assert_template_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("tagged-template-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("tagged-template fixture compiles");
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
        .expect("tagged-template fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}
