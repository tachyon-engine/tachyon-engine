use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const BIGINT_PRIMITIVE_SOURCE: &str = r#"
typeof 0n === "bigint"
    && !0n
    && !!1n
    && 140737488355327n === 140737488355327n
    && 140737488355328n === 140737488355328n
    && 140737488355328n !== 140737488355329n
    && (140737488355328n + "") === "140737488355328"
    && (340282366920938463463374607431768211455n + "")
        === "340282366920938463463374607431768211455"
    && -140737488355328n === -140737488355328n
    && (-18446744073709551617n + "") === "-18446744073709551617";
"#;

#[test]
fn bigint_primitives_execute_for_every_dispatch_batch() {
    assert_bigint_source::<1>(false);
    assert_bigint_source::<2>(false);
    assert_bigint_source::<4>(false);
    assert_bigint_source::<8>(false);
    assert_bigint_source::<16>(false);
}

#[test]
fn rooted_bigint_constants_survive_forced_major_collection() {
    assert_bigint_source::<1>(true);
    assert_bigint_source::<2>(true);
    assert_bigint_source::<4>(true);
    assert_bigint_source::<8>(true);
    assert_bigint_source::<16>(true);
}

/// Compiles and executes the primitive surface under one dispatch and collection policy.
fn assert_bigint_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_400 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("bigint-primitive-fixture"),
                MediaType::JavaScript,
                Arc::from(BIGINT_PRIMITIVE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("BigInt primitive fixture compiles");
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
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("BigInt primitive fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
