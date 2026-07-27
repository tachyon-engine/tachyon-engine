use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

#[path = "data_view_float16_surface.rs"]
mod float16_surface;

const DATA_VIEW_SOURCE: &str = r#"
var buffer = new ArrayBuffer(32);
var view = new DataView(buffer, 0, 32);
view.setUint32(16, 0x12345678);
view.setUint32(20, 0x12345678, true);
view.setInt16(24, -2);
view.setFloat32(26, 1.5, true);
view.setBigInt64(0, -1n, true);
view.setBigUint64(8, 18446744073709551615n);
var rangeThrows = false;
var brandThrows = false;
try { view.getFloat64(28); } catch (error) { rangeThrows = error instanceof RangeError; }
try { DataView.prototype.getUint8.call({}); } catch (error) {
  brandThrows = error instanceof TypeError;
}
view.buffer === buffer && view.byteOffset === 0 && view.byteLength === 32 &&
ArrayBuffer.isView(view) && Object.getPrototypeOf(view) === DataView.prototype &&
DataView.prototype.constructor === DataView &&
Object.prototype.toString.call(view) === "[object DataView]" &&
view.getUint8(16) === 0x12 && view.getUint8(17) === 0x34 &&
view.getUint8(20) === 0x78 && view.getUint8(23) === 0x12 &&
view.getUint32(16) === 0x12345678 && view.getUint32(20, true) === 0x12345678 &&
view.getInt16(24) === -2 && view.getFloat32(26, true) === 1.5 &&
view.getBigInt64(0, true) === -1n && view.getBigUint64(8) === 18446744073709551615n &&
new DataView(buffer, 1.9).byteOffset === 1 &&
DataView.name === "DataView" && DataView.length === 1 &&
DataView.prototype.getUint32.length === 1 && DataView.prototype.setUint32.length === 2 &&
rangeThrows && brandThrows;
"#;

#[test]
fn fixed_data_view_works_for_every_dispatch_batch() {
    assert_data_view_source::<1>(false);
    assert_data_view_source::<2>(false);
    assert_data_view_source::<4>(false);
    assert_data_view_source::<8>(false);
    assert_data_view_source::<16>(false);
}

#[test]
fn fixed_data_view_edges_survive_forced_major_collection() {
    assert_data_view_source::<8>(true);
}

/// Executes the shared endian, metadata, bounds, and brand fixture under one policy.
fn assert_data_view_source<const N: usize>(forced_major: bool) {
    let module = compile_data_view_fixture();
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
        .expect("DataView fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the shared fixture independently of dispatch and collection policy.
fn compile_data_view_fixture() -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_420),
                SourceName::new("data-view-fixture"),
                MediaType::JavaScript,
                Arc::from(DATA_VIEW_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("DataView fixture compiles")
}
