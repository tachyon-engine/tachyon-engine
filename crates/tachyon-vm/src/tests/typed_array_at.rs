use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_AT_SOURCE: &str = r#"
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var valuesOkay = true;
for (var index = 0; index < constructors.length; index++) {
  var TA = constructors[index];
  var array = new TA([10, 20, 30, 40]);
  valuesOkay = valuesOkay &&
    array.at(0) === 10 && array.at(3) === 40 &&
    array.at(-1) === 40 && array.at(-4) === 10 &&
    array.at(4) === undefined && array.at(-5) === undefined &&
    array.at(NaN) === 10 && array.at(undefined) === 10 &&
    array.at(0.9) === 10 && array.at(-0.9) === 10 &&
    array.at(Infinity) === undefined && array.at(-Infinity) === undefined &&
    array.at(0) === array.at(-0);
}

var callbackArray = new Uint8Array([1, 2, 3]);
var valueOfCalls = 0;
var callbackResult = callbackArray.at({
  valueOf: function() {
    valueOfCalls++;
    callbackArray[2] = 9;
    return -1;
  }
});
var symbolRejected = false;
try { callbackArray.at(Symbol("index")); } catch (error) {
  symbolRejected = error instanceof TypeError;
}
var brandRejected = false;
var brandConverted = false;
var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
try {
  typedArrayPrototype.at.call({}, {
    valueOf: function() { brandConverted = true; return 0; }
  });
} catch (error) {
  brandRejected = error instanceof TypeError;
}
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "at");
var lengthProperty = Object.getOwnPropertyDescriptor(property.value, "length");
var nameProperty = Object.getOwnPropertyDescriptor(property.value, "name");

valuesOkay && callbackResult === 9 && valueOfCalls === 1 &&
symbolRejected && brandRejected && !brandConverted &&
typeof property.value === "function" && property.writable === true &&
property.enumerable === false && property.configurable === true &&
property.value.length === 1 && lengthProperty.writable === false &&
lengthProperty.enumerable === false && lengthProperty.configurable === true &&
property.value.name === "at" && nameProperty.writable === false &&
nameProperty.enumerable === false && nameProperty.configurable === true;
"#;

const TYPED_ARRAY_AT_GC_SOURCE: &str = r#"
var array = new Uint8Array([1, 2, 3]);
var calls = 0;
var result = array.at({
  valueOf: function() {
    calls++;
    array[2] = 9;
    return -1;
  }
});
result === 9 && calls === 1;
"#;

#[test]
fn typed_array_at_works_for_every_dispatch_batch() {
    assert_typed_array_at::<1>(false);
    assert_typed_array_at::<2>(false);
    assert_typed_array_at::<4>(false);
    assert_typed_array_at::<8>(false);
    assert_typed_array_at::<16>(false);
}

#[test]
fn typed_array_at_receiver_survives_forced_major_conversion() {
    assert_typed_array_at::<8>(true);
}

/// Executes metadata, branding, numeric conversion, and callback revalidation under one policy.
fn assert_typed_array_at<const N: usize>(forced_major: bool) {
    let source = if forced_major {
        TYPED_ARRAY_AT_GC_SOURCE
    } else {
        TYPED_ARRAY_AT_SOURCE
    };
    let module = compile_typed_array_at_fixture(source);
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
        .expect("TypedArray at fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the shared at fixture independently of dispatch and collection policy.
fn compile_typed_array_at_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_424),
                SourceName::new("typed-array-at-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray at fixture compiles")
}
