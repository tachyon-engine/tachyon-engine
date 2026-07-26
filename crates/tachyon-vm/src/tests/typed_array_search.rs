use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_SEARCH_SOURCE: &str = r#"
function verifySearch(TA) {
  var array = new TA(5);
  array[0] = 10;
  array[1] = 20;
  array[2] = 10;
  array[3] = -0;
  array[4] = 40;
  return array.indexOf(10) === 0 && array.indexOf(10, 1) === 2 &&
    array.indexOf(10, -3) === 2 && array.indexOf(10, 99) === -1 &&
    array.indexOf(10, -99) === 0 && array.indexOf(10, Infinity) === -1 &&
    array.indexOf(10, -Infinity) === 0 && array.indexOf(10, NaN) === 0 &&
    array.lastIndexOf(10) === 2 && array.lastIndexOf(10, 1) === 0 &&
    array.lastIndexOf(10, -3) === 2 && array.lastIndexOf(10, -99) === -1 &&
    array.lastIndexOf(10, Infinity) === 2 &&
    array.lastIndexOf(10, -Infinity) === -1 &&
    array.indexOf(0) === 3 && array.indexOf(-0) === 3 &&
    array.lastIndexOf(0) === 3 && array.lastIndexOf(-0) === 3 &&
    array.indexOf("10") === -1 && array.lastIndexOf("10") === -1;
}
var valuesOkay = verifySearch(Float64Array) && verifySearch(Float32Array) &&
  verifySearch(Int32Array) && verifySearch(Int16Array) && verifySearch(Int8Array) &&
  verifySearch(Uint32Array) && verifySearch(Uint16Array) && verifySearch(Uint8Array) &&
  verifySearch(Uint8ClampedArray);

var floats = new Float64Array(3);
floats[0] = NaN;
floats[1] = 1;
floats[2] = NaN;
var strictOkay = floats.indexOf(NaN) === -1 && floats.lastIndexOf(NaN) === -1;
var searchCoerced = false;
var searchObject = { valueOf: function() { searchCoerced = true; return 2; } };
var strictArray = new Uint8Array(1);
strictArray[0] = 2;
strictOkay = strictOkay && strictArray.indexOf(searchObject) === -1 &&
  strictArray.lastIndexOf(searchObject) === -1 && !searchCoerced;

var callbackArray = new Uint8Array(4);
callbackArray[0] = 1;
callbackArray[1] = 2;
callbackArray[2] = 3;
callbackArray[3] = 2;
var forwardCalls = 0;
var forwardResult = callbackArray.indexOf(9, {
  valueOf: function() { forwardCalls++; callbackArray[2] = 9; return -2; }
});
var reverseCalls = 0;
var reverseResult = callbackArray.lastIndexOf(8, {
  valueOf: function() { reverseCalls++; callbackArray[1] = 8; return 2; }
});
var emptyConverted = false;
var empty = new Uint8Array(0);
var emptyOkay = empty.indexOf(0, {
  valueOf: function() { emptyConverted = true; return 0; }
}) === -1 && empty.lastIndexOf(0, {
  valueOf: function() { emptyConverted = true; return 0; }
}) === -1;

var brandConverted = false;
var brandRejected = 0;
var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
try { typedArrayPrototype.indexOf.call({}, 1, {
  valueOf: function() { brandConverted = true; return 0; }
}); } catch (error) { if (error instanceof TypeError) brandRejected++; }
try { typedArrayPrototype.lastIndexOf.call({}, 1, {
  valueOf: function() { brandConverted = true; return 0; }
}); } catch (error) { if (error instanceof TypeError) brandRejected++; }

var indexProperty = Object.getOwnPropertyDescriptor(typedArrayPrototype, "indexOf");
var lastProperty = Object.getOwnPropertyDescriptor(typedArrayPrototype, "lastIndexOf");
var metadataOkay = indexProperty.value.length === 1 &&
  indexProperty.value.name === "indexOf" && indexProperty.writable === true &&
  indexProperty.enumerable === false && indexProperty.configurable === true &&
  lastProperty.value.length === 1 && lastProperty.value.name === "lastIndexOf" &&
  lastProperty.writable === true && lastProperty.enumerable === false &&
  lastProperty.configurable === true;

valuesOkay && strictOkay && forwardResult === 2 && forwardCalls === 1 &&
reverseResult === 1 && reverseCalls === 1 && emptyOkay && !emptyConverted &&
brandRejected === 2 && !brandConverted && metadataOkay;
"#;

const TYPED_ARRAY_SEARCH_GC_SOURCE: &str = r#"
var array = new Uint8Array(4);
array[0] = 1;
array[1] = 2;
array[2] = 3;
array[3] = 2;
var forward = array.indexOf(9, {
  valueOf: function() { array[2] = 9; return -2; }
});
var reverse = array.lastIndexOf(8, {
  valueOf: function() { array[1] = 8; return 2; }
});
forward === 2 && reverse === 1;
"#;

#[test]
fn typed_array_search_works_for_every_dispatch_batch() {
    assert_typed_array_search::<1>(false);
    assert_typed_array_search::<2>(false);
    assert_typed_array_search::<4>(false);
    assert_typed_array_search::<8>(false);
    assert_typed_array_search::<16>(false);
}

#[test]
fn typed_array_search_state_survives_forced_major_conversion() {
    assert_typed_array_search::<8>(true);
}

/// Executes both directions, metadata, strict equality, and conversion under one policy.
fn assert_typed_array_search<const N: usize>(forced_major: bool) {
    let source = if forced_major {
        TYPED_ARRAY_SEARCH_GC_SOURCE
    } else {
        TYPED_ARRAY_SEARCH_SOURCE
    };
    let module = compile_typed_array_search_fixture(source);
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
        .expect("TypedArray search fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the shared bidirectional fixture independently of VM scheduling policy.
fn compile_typed_array_search_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_428),
                SourceName::new("typed-array-search-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray search fixture compiles")
}
