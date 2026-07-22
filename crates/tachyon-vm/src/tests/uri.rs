use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const URI_SOURCE: &str = r#"
var trace = "";
var object = {
  toString() { trace += "s"; return "a b"; },
  valueOf() { trace += "v"; return "wrong"; }
};
encodeURIComponent(object) === "a%20b" &&
trace === "s" &&
encodeURI("/é") === "/%C3%A9" &&
decodeURI("%2f%C3%A9") === "%2fé" &&
decodeURIComponent("%2f%C3%A9") === "/é";
"#;

const URI_ERROR_SOURCE: &str = r#"
var caught = false;
try {
  decodeURIComponent("%C0%80");
} catch (error) {
  caught = error instanceof URIError;
}
caught;
"#;

#[test]
fn uri_globals_resume_string_conversion_for_every_dispatch_batch() {
    assert_uri_source_batch::<1>(URI_SOURCE, 1_100);
    assert_uri_source_batch::<2>(URI_SOURCE, 1_101);
    assert_uri_source_batch::<4>(URI_SOURCE, 1_102);
    assert_uri_source_batch::<8>(URI_SOURCE, 1_103);
    assert_uri_source_batch::<16>(URI_SOURCE, 1_104);
}

#[test]
fn uri_globals_throw_uri_error_for_every_dispatch_batch() {
    assert_uri_source_batch::<1>(URI_ERROR_SOURCE, 1_105);
    assert_uri_source_batch::<2>(URI_ERROR_SOURCE, 1_106);
    assert_uri_source_batch::<4>(URI_ERROR_SOURCE, 1_107);
    assert_uri_source_batch::<8>(URI_ERROR_SOURCE, 1_108);
    assert_uri_source_batch::<16>(URI_ERROR_SOURCE, 1_109);
}

/// Compiles and executes one URI fixture under a selected dispatch monomorphization.
fn assert_uri_source_batch<const N: usize>(source: &str, source_id: u32) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("uri-global-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("URI fixture compiles");
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("URI fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}
