use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;
use crate::tests::fixtures::test_isolate;

const DEFINE_PROPERTIES_SOURCE: &str = r#"
var effects = "";
var descriptors = {
    get answer() {
        effects = effects + "m";
        return {
            get value() { effects = effects + "v"; return 42; },
            enumerable: true,
            configurable: true,
            writable: true
        };
    }
};
var target = {};
var result = Object.defineProperties(target, descriptors);
result === target && effects === "mv" && target.answer === 42;
"#;

#[test]
fn object_define_properties_getters_are_stable_across_dispatch_batches() {
    assert_define_properties_batch::<1>(280);
    assert_define_properties_batch::<2>(281);
    assert_define_properties_batch::<4>(282);
    assert_define_properties_batch::<8>(283);
    assert_define_properties_batch::<16>(284);
}

/// Compiles and executes the nested descriptor-getter fixture for one dispatch batch.
fn assert_define_properties_batch<const N: usize>(source_id: u32) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("object-define-properties"),
                MediaType::JavaScript,
                Arc::from(DEFINE_PROPERTIES_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Object.defineProperties fixture compiles");
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("Object.defineProperties fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}
