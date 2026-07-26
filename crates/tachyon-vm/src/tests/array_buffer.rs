use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_BUFFER_SOURCE: &str = r#"
var b = new ArrayBuffer(8);
b.byteLength === 8 && b.maxByteLength === 8 && !b.resizable && !b.detached &&
  ArrayBuffer.isView(b) === false && Object.getPrototypeOf(b) === ArrayBuffer.prototype &&
  ArrayBuffer.prototype.constructor === ArrayBuffer &&
  Object.prototype.toString.call(b) === "[object ArrayBuffer]";
"#;

const ARRAY_BUFFER_DETACH_SOURCE: &str = r#"
var buffer = new ArrayBuffer(16);
var typed = new Uint8Array(buffer, 4, 4);
var view = new DataView(buffer, 2, 8);
typed[0] = 23;
var detachResult = $262.detachArrayBuffer(buffer);
var dataViewLengthThrows = false;
var dataViewOffsetThrows = false;
var dataViewReadThrows = false;
var dataViewIndexThrowsFirst = false;
var constructorIndexThrowsFirst = false;
try { view.byteLength; } catch (error) { dataViewLengthThrows = error instanceof TypeError; }
try { view.byteOffset; } catch (error) { dataViewOffsetThrows = error instanceof TypeError; }
try { view.getUint8(13); } catch (error) { dataViewReadThrows = error instanceof TypeError; }
try { view.getUint8(Infinity); } catch (error) { dataViewIndexThrowsFirst = error instanceof RangeError; }
try { new DataView(buffer, Infinity); } catch (error) {
  constructorIndexThrowsFirst = error instanceof RangeError;
}
var invalidThrows = false;
try { $262.detachArrayBuffer({}); } catch (error) { invalidThrows = error instanceof TypeError; }
$262.detachArrayBuffer(buffer);

var duringAtBuffer = new ArrayBuffer(4);
var duringAt = new Uint8Array(duringAtBuffer);
duringAt[0] = 7;
var atResult = duringAt.at({
  valueOf: function() { $262.detachArrayBuffer(duringAtBuffer); return 0; }
});
var duringIncludesBuffer = new ArrayBuffer(4);
var duringIncludes = new Uint8Array(duringIncludesBuffer);
var includesResult = duringIncludes.includes(undefined, {
  valueOf: function() { $262.detachArrayBuffer(duringIncludesBuffer); return 0; }
});

detachResult === undefined && buffer.detached && buffer.byteLength === 0 &&
buffer.maxByteLength === 0 && buffer.resizable === false &&
typed.buffer === buffer && typed.length === 0 && typed.byteLength === 0 &&
typed.byteOffset === 0 && typed[0] === undefined &&
view.buffer === buffer && dataViewLengthThrows && dataViewOffsetThrows &&
dataViewReadThrows && dataViewIndexThrowsFirst && constructorIndexThrowsFirst &&
invalidThrows && atResult === undefined && includesResult === true;
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

#[test]
fn detach_is_observed_by_every_fixed_view_for_dispatch_batches() {
    assert_array_buffer_detach::<1>(false);
    assert_array_buffer_detach::<2>(false);
    assert_array_buffer_detach::<4>(false);
    assert_array_buffer_detach::<8>(false);
    assert_array_buffer_detach::<16>(false);
}

#[test]
fn detach_edges_survive_forced_major_collection() {
    assert_array_buffer_detach::<8>(true);
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

/// Executes the host detach, view observation, ordering, and idempotence fixture.
fn assert_array_buffer_detach<const N: usize>(forced_major: bool) {
    let module = compile_source(ARRAY_BUFFER_DETACH_SOURCE, 7_411);
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(unused_eval_callback, unused_dynamic_function_callback)
        .expect("detach host hook installs");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .expect("ArrayBuffer detach fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

fn unused_eval_callback(
    _isolate: &mut Isolate,
    _realm: RealmId,
    _kind: EvalKind,
    _source: Value,
) -> Result<Value, ExecutionError> {
    Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
}

fn unused_dynamic_function_callback(
    _isolate: &mut Isolate,
    _realm: RealmId,
) -> Result<Value, ExecutionError> {
    Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
}

/// Compiles the shared fixture independently of dispatch and collection policy.
fn compile_array_buffer_fixture() -> CompiledModule {
    compile_source(ARRAY_BUFFER_SOURCE, 7_410)
}

/// Compiles one ArrayBuffer fixture independently of dispatch and collection policy.
fn compile_source(source: &'static str, id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(id),
                SourceName::new("array-buffer-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("ArrayBuffer fixture compiles")
}
use std::sync::Arc;
