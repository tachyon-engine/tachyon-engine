use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const REGEXP_ESCAPE_SOURCE: &str = r#"
var typeError = false;
try { RegExp.escape(new String("x")); } catch (error) { typeError = error instanceof TypeError; }
RegExp.escape("foo.bar") === "\\x66oo\\.bar" &&
RegExp.escape("(a)-b/c") === "\\(a\\)\\x2db\\/c" &&
RegExp.escape("\t\n \u2028") === "\\t\\n\\x20\\u2028" &&
RegExp.escape(String.fromCharCode(0xD800)) === "\\ud800" &&
RegExp.escape("\uD800") === "\\ud800" &&
RegExp.escape("\u{1F600}") === "\u{1F600}" &&
RegExp.escape.name === "escape" && RegExp.escape.length === 1 && typeError;
"#;

#[test]
fn regexp_escape_matches_ecmascript_for_every_dispatch_batch() {
    assert_regexp_escape::<1>(false);
    assert_regexp_escape::<2>(false);
    assert_regexp_escape::<4>(false);
    assert_regexp_escape::<8>(false);
    assert_regexp_escape::<16>(false);
}

#[test]
fn regexp_escape_output_survives_forced_major_collection() {
    assert_regexp_escape::<8>(true);
}

/// Compiles and executes the UTF-16 and builtin-contract fixture under one batch size.
fn assert_regexp_escape<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(1_180 + N as u32),
                SourceName::new("regexp-escape-fixture"),
                MediaType::JavaScript,
                Arc::from(REGEXP_ESCAPE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("RegExp.escape fixture compiles");
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
        .expect("RegExp.escape fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}
