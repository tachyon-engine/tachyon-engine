use super::*;

const DATA_VIEW_FLOAT16_SOURCE: &str = r#"
var buffer = new ArrayBuffer(16);
var view = new DataView(buffer);
var constructorThrows = false;
try { new DataView.prototype.getFloat16(); } catch (error) {
  constructorThrows = error instanceof TypeError;
}

view.setFloat16(0, 42);
view.setFloat16(2, 42, true);
view.setFloat16(4, 1.00048828125);
view.setFloat16(6, 5.960464477539063e-8);
view.setFloat16(8, 65520);
view.setFloat16(10, NaN);

DataView.prototype.getFloat16.name === "getFloat16" &&
DataView.prototype.getFloat16.length === 1 &&
DataView.prototype.setFloat16.name === "setFloat16" &&
DataView.prototype.setFloat16.length === 2 &&
view.getUint16(0) === 0x5140 && view.getFloat16(0) === 42 &&
view.getUint16(2, true) === 0x5140 && view.getFloat16(2, true) === 42 &&
view.getUint16(4) === 0x3c00 && view.getFloat16(4) === 1 &&
view.getUint16(6) === 1 && view.getFloat16(6) === 5.960464477539063e-8 &&
view.getUint16(8) === 0x7c00 && view.getFloat16(8) === Infinity &&
view.getFloat16(10) !== view.getFloat16(10) && constructorThrows;
"#;

#[test]
fn data_view_float16_works_for_every_dispatch_batch() {
    assert_float16_surface::<1>(false);
    assert_float16_surface::<2>(false);
    assert_float16_surface::<4>(false);
    assert_float16_surface::<8>(false);
    assert_float16_surface::<16>(false);
}

#[test]
fn data_view_float16_survives_forced_major_collection() {
    assert_float16_surface::<8>(true);
}

/// Executes the Float16 surface fixture under one dispatch and collection policy.
fn assert_float16_surface<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_421),
                SourceName::new("data-view-float16-fixture"),
                MediaType::JavaScript,
                Arc::from(DATA_VIEW_FLOAT16_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("DataView Float16 fixture compiles");
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
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .expect("DataView Float16 fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
