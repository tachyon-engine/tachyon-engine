use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_TRANSFORM_SOURCE: &str = r#"
function throwsTypeError(callback) {
  try { callback(); return false; }
  catch (error) { return error instanceof TypeError; }
}

var receiver = new Uint8Array([1, 2, 3]);
var thisArg = { marker: true };
var callbackContract = true;
var mapped = receiver.map(function(value, index, original) {
  callbackContract = callbackContract && this === thisArg && original === receiver &&
    value === index + 1;
  return { valueOf: function() { return value * 2; } };
}, thisArg);
var filtered = receiver.filter(function(value, index, original) {
  callbackContract = callbackContract && this === thisArg && original === receiver;
  return index !== 1;
}, thisArg);

var constructors = [
  Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array,
  Int32Array, Uint32Array, Float32Array, Float64Array, BigInt64Array, BigUint64Array
];
var allKinds = true;
for (var kindIndex = 0; kindIndex < constructors.length; kindIndex++) {
  var TA = constructors[kindIndex];
  var bigint = TA === BigInt64Array || TA === BigUint64Array;
  var one = bigint ? 1n : 1;
  var two = bigint ? 2n : 2;
  var three = bigint ? 3n : 3;
  var source = new TA([one, two, three]);
  var kindMapped = source.map(function(value) { return value; });
  var kindFiltered = source.filter(function(value, index) { return index !== 1; });
  allKinds = allKinds && Object.getPrototypeOf(kindMapped) === TA.prototype &&
    Object.getPrototypeOf(kindFiltered) === TA.prototype && kindMapped.length === 3 &&
    kindMapped[0] === one && kindMapped[2] === three && kindFiltered.length === 2 &&
    kindFiltered[0] === one && kindFiltered[1] === three;
}

var mapSpeciesLength = -1;
var mapSpeciesSource = new Uint8Array([4, 5]);
mapSpeciesSource.constructor = {
  [Symbol.species]: function(length) {
    mapSpeciesLength = length;
    return new Int16Array(length + 1);
  }
};
var mapSpeciesResult = mapSpeciesSource.map(function(value) { return value + 1; });

var order = "";
var filterSpeciesLength = -1;
var filterSpeciesSource = new Uint8Array([1, 2, 3]);
filterSpeciesSource.constructor = {
  get [Symbol.species]() {
    order += "s";
    return function(length) {
      order += "c";
      filterSpeciesLength = length;
      return new Uint16Array(length);
    };
  }
};
var filterSpeciesResult = filterSpeciesSource.filter(function(value) {
  order += value;
  return value !== 2;
});

var detachedSource = new Uint8Array([1, 2, 3]);
var detachedCalls = 0;
var detachedMapped = detachedSource.map(function(value) {
  detachedCalls++;
  if (detachedCalls === 1) detachedSource.buffer.transfer();
  return value === undefined ? 9 : value;
});

var detachedTarget;
var targetSource = new Uint8Array([1, 2]);
targetSource.constructor = {
  [Symbol.species]: function(length) {
    detachedTarget = new Uint8Array(length);
    return detachedTarget;
  }
};
var detachedTargetResult = targetSource.map(function(value, index) {
  if (index === 0) detachedTarget.buffer.transfer();
  return value;
});

var bigintObjectMapped = new BigInt64Array([1n]).map(function() {
  return { valueOf: function() { return 7n; } };
});
var bigintFilteredByValue = new BigInt64Array([41n, 1n, 42n, 7n]).filter(function(value) {
  return value > 40n;
});
var mapMismatch = throwsTypeError(function() {
  var value = new BigInt64Array(0);
  value.constructor = { [Symbol.species]: Int8Array };
  value.map(function(entry) { return entry; });
});
var filterMismatch = throwsTypeError(function() {
  var value = new BigInt64Array(0);
  value.constructor = { [Symbol.species]: Int8Array };
  value.filter(function() { return true; });
});
var shortMap = throwsTypeError(function() {
  var value = new Uint8Array([1]);
  value.constructor = { [Symbol.species]: function() { return new Uint8Array(0); } };
  value.map(function(entry) { return entry; });
});
var shortFilter = throwsTypeError(function() {
  var value = new Uint8Array([1]);
  value.constructor = { [Symbol.species]: function() { return new Uint8Array(0); } };
  value.filter(function() { return true; });
});

callbackContract && mapped.join(",") === "2,4,6" && filtered.join(",") === "1,3" &&
allKinds && mapSpeciesLength === 2 && mapSpeciesResult instanceof Int16Array &&
mapSpeciesResult.length === 3 && mapSpeciesResult[0] === 5 && mapSpeciesResult[1] === 6 &&
order === "123sc" && filterSpeciesLength === 2 && filterSpeciesResult instanceof Uint16Array &&
filterSpeciesResult.join(",") === "1,3" && detachedCalls === 3 &&
detachedMapped.join(",") === "1,9,9" && detachedTargetResult === detachedTarget &&
detachedTarget.length === 0 && bigintObjectMapped[0] === 7n &&
bigintFilteredByValue.length === 2 && bigintFilteredByValue[0] === 41n &&
bigintFilteredByValue[1] === 42n && mapMismatch && filterMismatch &&
shortMap && shortFilter && Uint8Array.prototype.map.length === 1 &&
Uint8Array.prototype.filter.length === 1;
"#;

#[test]
fn typed_array_map_filter_work_for_every_dispatch_batch() {
    assert_typed_array_transform::<1>(false);
    assert_typed_array_transform::<2>(false);
    assert_typed_array_transform::<4>(false);
    assert_typed_array_transform::<8>(false);
    assert_typed_array_transform::<16>(false);
}

#[test]
fn typed_array_map_filter_survive_forced_major_collection() {
    assert_typed_array_transform::<1>(true);
    assert_typed_array_transform::<2>(true);
    assert_typed_array_transform::<4>(true);
    assert_typed_array_transform::<8>(true);
    assert_typed_array_transform::<16>(true);
}

/// Compiles and executes the shared transform fixture under one VM dispatch/GC policy.
fn assert_typed_array_transform<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(8_100 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("typed-array-transform-fixture"),
                MediaType::JavaScript,
                Arc::from(TYPED_ARRAY_TRANSFORM_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray transform fixture compiles");
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
                fuel: 524_288,
                quantum: 524_288,
            },
        )
        .expect("TypedArray transform fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}
