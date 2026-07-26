use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_INCLUDES_SOURCE: &str = r#"
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var valuesOkay = true;
for (var index = 0; index < constructors.length; index++) {
  var TA = constructors[index];
  var array = new TA([10, 20, 30, 40]);
  valuesOkay = valuesOkay &&
    array.includes(10) && array.includes(40) && !array.includes(50) &&
    !array.includes(10, 1) && array.includes(20, 1) &&
    array.includes(40, -1) && array.includes(10, -99) &&
    array.includes(10, NaN) && array.includes(10, -Infinity) &&
    !array.includes(10, Infinity) && array.includes(10, 0.9) &&
    array.includes(10, -0.9) && array.includes(10, -0) &&
    !array.includes("10") && !array.includes({
      valueOf: function() { throw 42; }
    });
}

var floatArray = new Float64Array([NaN, -0, Infinity]);
var sameValueZeroOkay = floatArray.includes(NaN) &&
  floatArray.includes(0) && floatArray.includes(-0) && floatArray.includes(Infinity);
var callbackArray = new Uint8Array([1, 2, 3]);
var valueOfCalls = 0;
var callbackResult = callbackArray.includes(9, {
  valueOf: function() {
    valueOfCalls++;
    callbackArray[2] = 9;
    return -1;
  }
});
var emptyConverted = false;
var emptyResult = new Uint8Array(0).includes(0, {
  valueOf: function() { emptyConverted = true; return 0; }
});
var symbolRejected = false;
try { callbackArray.includes(1, Symbol("index")); } catch (error) {
  symbolRejected = error instanceof TypeError;
}
var brandRejected = false;
var brandConverted = false;
var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
try {
  typedArrayPrototype.includes.call({}, 1, {
    valueOf: function() { brandConverted = true; return 0; }
  });
} catch (error) {
  brandRejected = error instanceof TypeError;
}
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "includes");
var lengthProperty = Object.getOwnPropertyDescriptor(property.value, "length");
var nameProperty = Object.getOwnPropertyDescriptor(property.value, "name");

valuesOkay && sameValueZeroOkay && callbackResult && valueOfCalls === 1 &&
emptyResult === false && !emptyConverted && symbolRejected &&
brandRejected && !brandConverted && typeof property.value === "function" &&
property.writable === true && property.enumerable === false &&
property.configurable === true && property.value.length === 1 &&
lengthProperty.writable === false && lengthProperty.enumerable === false &&
lengthProperty.configurable === true && property.value.name === "includes" &&
nameProperty.writable === false && nameProperty.enumerable === false &&
nameProperty.configurable === true;
"#;

const TYPED_ARRAY_INCLUDES_GC_SOURCE: &str = r#"
var array = new Uint8Array([1, 2, 3]);
var calls = 0;
var result = array.includes(9, {
  valueOf: function() {
    calls++;
    array[2] = 9;
    return -1;
  }
});
result && calls === 1;
"#;

#[test]
fn typed_array_includes_works_for_every_dispatch_batch() {
    assert_typed_array_includes::<1>(false);
    assert_typed_array_includes::<2>(false);
    assert_typed_array_includes::<4>(false);
    assert_typed_array_includes::<8>(false);
    assert_typed_array_includes::<16>(false);
}

#[test]
fn typed_array_includes_state_survives_forced_major_conversion() {
    assert_typed_array_includes::<8>(true);
}

/// Executes fixed Number search, metadata, branding, and conversion under one policy.
fn assert_typed_array_includes<const N: usize>(forced_major: bool) {
    let source = if forced_major {
        TYPED_ARRAY_INCLUDES_GC_SOURCE
    } else {
        TYPED_ARRAY_INCLUDES_SOURCE
    };
    let module = compile_typed_array_includes_fixture(source);
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
        .expect("TypedArray includes fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the selected includes fixture independently of dispatch and collection policy.
fn compile_typed_array_includes_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_426),
                SourceName::new("typed-array-includes-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray includes fixture compiles")
}
