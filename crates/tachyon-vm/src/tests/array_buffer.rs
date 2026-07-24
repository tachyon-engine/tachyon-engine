use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_BUFFER_SOURCE: &str = r#"
var b = new ArrayBuffer(8);
b.byteLength === 8 && b.maxByteLength === 8 && !b.resizable && !b.detached &&
  ArrayBuffer.isView(b) === false && Object.getPrototypeOf(b) === ArrayBuffer.prototype &&
  ArrayBuffer.prototype.constructor === ArrayBuffer &&
  Object.prototype.toString.call(b) === "[object ArrayBuffer]";
"#;

#[test]
fn array_buffer_fixed_constructor_and_accessors_work_for_dispatch_batches() {
    assert_array_buffer_source::<1>();
    assert_array_buffer_source::<2>();
    assert_array_buffer_source::<4>();
    assert_array_buffer_source::<8>();
    assert_array_buffer_source::<16>();
}

#[test]
fn array_buffer_backing_survives_forced_major_collection() {
    let module = compile_array_buffer_fixture();
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("ArrayBuffer fixture survives forced major GC");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

/// Compiles and runs the fixed ArrayBuffer fixture under one dispatch policy.
fn assert_array_buffer_source<const N: usize>() {
    let module = compile_array_buffer_fixture();
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("ArrayBuffer fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles the shared fixture independently of dispatch and collection policy.
fn compile_array_buffer_fixture() -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_410),
                SourceName::new("array-buffer-fixture"),
                MediaType::JavaScript,
                Arc::from(ARRAY_BUFFER_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("ArrayBuffer fixture compiles")
}
use std::sync::Arc;
