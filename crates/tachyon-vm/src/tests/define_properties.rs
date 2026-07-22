use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

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
var atomic = {};
try {
    Object.defineProperties(atomic, {
        first: { value: 1 },
        get second() { throw 1; }
    });
} catch (error) {}
result === target && effects === "mv" && target.answer === 42 &&
    !Object.hasOwn(atomic, "first");
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
    let mut isolate = define_properties_test_isolate();
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

fn define_properties_test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("Object.defineProperties test isolate initializes")
}
