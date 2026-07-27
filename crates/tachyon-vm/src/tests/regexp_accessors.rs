use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const REGEXP_ACCESSOR_SOURCE: &str = r#"
var sourceDescriptor = Object.getOwnPropertyDescriptor(RegExp.prototype, "source");
var flagsDescriptor = Object.getOwnPropertyDescriptor(RegExp.prototype, "flags");
var globalDescriptor = Object.getOwnPropertyDescriptor(RegExp.prototype, "global");
var regexp = new RegExp("/\n\u2028", "gimsy");
var invalidSource = false;
try { sourceDescriptor.get.call({}); } catch (error) { invalidSource = error instanceof TypeError; }
var customFlags = flagsDescriptor.get.call({
  hasIndices: 1,
  global: "yes",
  ignoreCase: {},
  multiline: [],
  dotAll: Symbol(),
  unicode: true,
  unicodeSets: 1,
  sticky: 42
});
sourceDescriptor.get.name === "get source" && sourceDescriptor.get.length === 0 &&
flagsDescriptor.get.name === "get flags" && flagsDescriptor.get.length === 0 &&
sourceDescriptor.enumerable === false && sourceDescriptor.configurable === true &&
sourceDescriptor.set === undefined && !Object.prototype.hasOwnProperty.call(regexp, "source") &&
regexp.source === "\\/\\n\\u2028" && regexp.flags === "gimsy" && customFlags === "dgimsuvy" &&
globalDescriptor.get.call(regexp) === true &&
globalDescriptor.get.call(RegExp.prototype) === undefined && invalidSource;
"#;

#[test]
fn regexp_accessors_work_for_every_dispatch_batch() {
    assert_regexp_accessors::<1>(false);
    assert_regexp_accessors::<2>(false);
    assert_regexp_accessors::<4>(false);
    assert_regexp_accessors::<8>(false);
    assert_regexp_accessors::<16>(false);
}

#[test]
fn regexp_accessors_survive_forced_major_collection() {
    assert_regexp_accessors::<8>(true);
}

/// Compiles and executes the accessor contract under one dispatch batch and GC mode.
fn assert_regexp_accessors<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(1_190 + N as u32),
                SourceName::new("regexp-accessor-fixture"),
                MediaType::JavaScript,
                Arc::from(REGEXP_ACCESSOR_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("RegExp accessor fixture compiles");
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
        .expect("RegExp accessor fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}
