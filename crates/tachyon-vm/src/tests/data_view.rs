use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const DATA_VIEW_SOURCE: &str = r#"
var buffer = new ArrayBuffer(16);
var view = new DataView(buffer, 0, 16);
view.setUint32(0, 0x12345678);
view.setUint32(4, 0x12345678, true);
view.setInt16(8, -2);
view.setFloat32(10, 1.5, true);
var rangeThrows = false;
var brandThrows = false;
try { view.getFloat64(12); } catch (error) { rangeThrows = error instanceof RangeError; }
try { DataView.prototype.getUint8.call({}); } catch (error) {
  brandThrows = error instanceof TypeError;
}
view.buffer === buffer && view.byteOffset === 0 && view.byteLength === 16 &&
ArrayBuffer.isView(view) && Object.getPrototypeOf(view) === DataView.prototype &&
DataView.prototype.constructor === DataView &&
Object.prototype.toString.call(view) === "[object DataView]" &&
view.getUint8(0) === 0x12 && view.getUint8(1) === 0x34 &&
view.getUint8(4) === 0x78 && view.getUint8(7) === 0x12 &&
view.getUint32(0) === 0x12345678 && view.getUint32(4, true) === 0x12345678 &&
view.getInt16(8) === -2 && view.getFloat32(10, true) === 1.5 &&
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
