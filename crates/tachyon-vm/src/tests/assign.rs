use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;
use crate::tests::fixtures::test_isolate;

const ASSIGN_SOURCE: &str = r#"
var effects = "";
var target = {
    set x(value) { effects = effects + "s" + value; }
};
var first = {
    get x() { effects = effects + "g"; return 1; }
};
var result = Object.assign(target, first, { y: 2 }, "ab");
result === target && effects === "gs1" && result.y === 2 &&
    result[0] === "a" && result[1] === "b";
"#;

#[test]
fn object_assign_continuations_are_stable_across_dispatch_batches() {
    assert_assign_batch::<1>(260);
    assert_assign_batch::<2>(261);
    assert_assign_batch::<4>(262);
    assert_assign_batch::<8>(263);
    assert_assign_batch::<16>(264);
}

/// Compiles and executes the multi-source getter/setter fixture for one dispatch batch.
fn assert_assign_batch<const N: usize>(source_id: u32) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("object-assign"),
                MediaType::JavaScript,
                Arc::from(ASSIGN_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Object.assign fixture compiles");
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("Object.assign fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}
