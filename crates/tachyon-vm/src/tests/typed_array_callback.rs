use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_CALLBACK_SOURCE: &str = r#"
function verify(TA) {
  var array = new TA([1, 2, 3, 4]);
  var seen = "";
  var thisArg = { marker: 9 };
  var every = array.every(function(value, index, receiver) {
    seen += value + ":" + index + ";";
    return this === thisArg && receiver === array && value < 5;
  }, thisArg);
  var someCalls = 0;
  var some = array.some(function(value) { someCalls++; return value === 2; });
  var find = array.find(function(value) { return value > 2; });
  var findIndex = array.findIndex(function(value) { return value > 2; });
  var reverseSeen = "";
  var findLast = array.findLast(function(value, index) {
    reverseSeen += index;
    return value < 4;
  });
  var findLastIndex = array.findLastIndex(function(value) { return value < 4; });
  return every && seen === "1:0;2:1;3:2;4:3;" && some && someCalls === 2 &&
    find === 3 && findIndex === 2 && findLast === 3 && findLastIndex === 2 &&
    reverseSeen === "32";
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var valuesOkay = true;
for (var i = 0; i < constructors.length; i++) valuesOkay = valuesOkay && verify(constructors[i]);

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var names = [
  "every", "some", "find", "findIndex", "findLast", "findLastIndex",
  "forEach", "reduce", "reduceRight"
];
var metadataOkay = true;
for (var j = 0; j < names.length; j++) {
  var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, names[j]);
  metadataOkay = metadataOkay && property.value.name === names[j] && property.value.length === 1 &&
    property.writable === true && property.enumerable === false && property.configurable === true;
}
var rejected = 0;
for (var k = 0; k < names.length; k++) {
  try { typedArrayPrototype[names[k]].call({}, function() { return true; }); }
  catch (error) { if (error instanceof TypeError) rejected++; }
}
var nonCallable = false;
try { new Uint8Array(0).every(1); } catch (error) { nonCallable = error instanceof TypeError; }
var detached = new Uint8Array([5, 6]);
var detachedCalls = 0;
var detachedSecond = 1;
detached.every(function(value, index, receiver) {
  if (index === 0) $262.detachArrayBuffer(receiver.buffer);
  else detachedSecond = value;
  detachedCalls++;
  return true;
});
var array = new Uint8Array([1, 2, 3, 4]);
var thisArg = { marker: 9 };
var forEachSeen = "";
var forEachResult = array.forEach(function(value, index, receiver) {
  forEachSeen += value + ":" + index + ":" + (receiver === array) + ":" + (this === thisArg) + ";";
}, thisArg);
var reduceCalls = 0;
var reduceResult = array.reduce(function(accumulator, value, index, receiver) {
  "use strict";
  reduceCalls++;
  return accumulator + value + index + (receiver === array ? 0 : 100) + (this === undefined ? 0 : 100);
}, 10);
var reduceRightSeen = "";
var reduceRightResult = array.reduceRight(function(accumulator, value, index, receiver) {
  "use strict";
  reduceRightSeen += index;
  return accumulator - value;
});
var omittedCalls = 0;
var omitted = new Uint8Array([9]).reduce(function() { omittedCalls++; });
var explicitUndefinedCalls = 0;
var explicitUndefined = new Uint8Array([7]).reduce(function(accumulator, value) {
  explicitUndefinedCalls++;
  return accumulator === undefined ? value : 99;
}, undefined);
var reverseOmittedCalls = 0;
var reverseOmitted = new Uint8Array([11]).reduceRight(function() { reverseOmittedCalls++; });
var reverseExplicitCalls = 0;
var reverseExplicit = new Uint8Array([12]).reduceRight(function(accumulator, value) {
  reverseExplicitCalls++;
  return accumulator === undefined ? value : 99;
}, undefined);
var emptyExplicit = new Uint8Array(0).reduce(function() { throw 1; }, undefined);
var emptyOmitted = false;
try { new Uint8Array(0).reduce(function() {}); }
catch (error) { emptyOmitted = error instanceof TypeError; }
var emptyRightOmitted = false;
try { new Uint8Array(0).reduceRight(function() {}); }
catch (error) { emptyRightOmitted = error instanceof TypeError; }
var abrupt = {};
var abruptIdentity = false;
try { array.forEach(function() { throw abrupt; }); }
catch (error) { abruptIdentity = error === abrupt; }
valuesOkay && metadataOkay && rejected === 9 && nonCallable &&
  detachedCalls === 2 && detachedSecond === undefined &&
  forEachResult === undefined && forEachSeen === "1:0:true:true;2:1:true:true;3:2:true:true;4:3:true:true;" &&
  reduceResult === 26 && reduceCalls === 4 && reduceRightResult === -2 && reduceRightSeen === "210" &&
  omitted === 9 && omittedCalls === 0 && explicitUndefined === 7 && explicitUndefinedCalls === 1 &&
  reverseOmitted === 11 && reverseOmittedCalls === 0 && reverseExplicit === 12 && reverseExplicitCalls === 1 &&
  emptyExplicit === undefined && emptyOmitted && emptyRightOmitted && abruptIdentity;
"#;

const TYPED_ARRAY_CALLBACK_GC_SOURCE: &str = r#"
var array = new Uint8Array([1, 2, 3, 4]);
var seen = 0;
var reflectOkay = true;
var result = array.find(function(value, index, receiver) {
  seen += value + index;
  reflectOkay = Reflect.set(receiver, index, value + 1) && reflectOkay;
  return index === 2;
});
var detached = new Uint8Array([5, 6]);
var detachedCalls = 0;
var detachedSecond = 1;
detached.some(function(value, index, receiver) {
  if (index === 0) $262.detachArrayBuffer(receiver.buffer);
  else detachedSecond = value;
  detachedCalls++;
  return false;
});
var forEachDetached = new Uint8Array([7, 8, 9]);
var forEachDetachedSeen = "";
forEachDetached.forEach(function(value, index, receiver) {
  if (index === 0) $262.detachArrayBuffer(receiver.buffer);
  forEachDetachedSeen += String(value) + ";";
});
var reduceDetached = new Uint8Array([10, 20, 30]);
var reduceDetachedSeen = "";
var reduceDetachedResult = reduceDetached.reduce(function(accumulator, value, index, receiver) {
  if (index === 0) $262.detachArrayBuffer(receiver.buffer);
  reduceDetachedSeen += String(value) + ";";
  return accumulator;
}, { rooted: true });
var reducedObject = array.reduce(function(accumulator, value) {
  return { total: accumulator.total + value };
}, { total: 0 });
var live = new Uint8Array([1, 2, 3]);
var liveSeen = "";
live.reduce(function(accumulator, value, index, receiver) {
  liveSeen += value;
  if (index === 0) reflectOkay = Reflect.set(receiver, 1, 9) && reflectOkay;
  return accumulator;
}, 0);
result === 3 && seen === 9 && reflectOkay && detachedCalls === 2 && detachedSecond === undefined &&
  forEachDetachedSeen === "7;undefined;undefined;" &&
  reduceDetachedSeen === "10;undefined;undefined;" && reduceDetachedResult.rooted === true &&
  reducedObject.total === 13 && liveSeen === "193";
"#;

const TYPED_ARRAY_CALLBACK_LONG_SOURCE: &str = r#"
var length = 20000;
var array = new Uint8Array(length);
var calls = 0;
var result = array.reduce(function(accumulator, value, index, receiver) {
  calls++;
  return accumulator + (receiver === array && value === 0 && index < length ? 1 : 0);
}, 0);
result === length && calls === length;
"#;

const TYPED_ARRAY_REFLECT_SET_SOURCE: &str = r#"
function throwsTypeError(callback) {
  try { callback(); return false; }
  catch (error) { return error instanceof TypeError; }
}
var array = new Uint8Array([1, 2]);
var direct = Reflect.set(array, "0", 9) && array[0] === 9;
var ordinaryKey = Reflect.set(array, "01", 17) && array["01"] === 17;
var invalidKeys = ["-0", "-1", "NaN", "Infinity", "1.5", "4"];
var invalidOkay = true;
for (var i = 0; i < invalidKeys.length; i++) {
  var key = invalidKeys[i];
  invalidOkay = Reflect.set(array, key, 23) &&
    Object.getOwnPropertyDescriptor(array, key) === undefined && invalidOkay;
}
var receiver = {};
var receiverOkay = Reflect.set(array, "1", 31, receiver) &&
  receiver[1] === 31 && array[1] === 2;
var other = new Uint8Array(2);
var typedReceiverOkay = Reflect.set(array, "1", 41, other) &&
  other[1] === 41 && array[1] === 2;
var boxedReceiver = new Float64Array(1);
var boxedReceiverOkay = Reflect.set(array, "0", new Number(2.3), boxedReceiver) &&
  boxedReceiver[0] === 2.3 && array[0] === 9;
var shortReceiver = new Uint8Array(1);
var shortReceiverOkay = Reflect.set(array, "1", 51, shortReceiver) === false &&
  shortReceiver[1] === undefined;
var invalidReceiver = {};
var invalidTargetOkay = Reflect.set(array, "8", 61, invalidReceiver) &&
  Object.getOwnPropertyDescriptor(invalidReceiver, "8") === undefined;
var mismatch = throwsTypeError(function() { Reflect.set(array, "0", 1n); });
var big = new BigInt64Array([1n]);
var bigOkay = Reflect.set(big, "0", 7n) && big[0] === 7n &&
  throwsTypeError(function() { Reflect.set(big, "0", 1); });
var detached = new Uint8Array([1]);
$262.detachArrayBuffer(detached.buffer);
var detachedOkay = Reflect.set(detached, "0", 71) && detached[0] === undefined;
direct && ordinaryKey && invalidOkay && receiverOkay && typedReceiverOkay && boxedReceiverOkay &&
  shortReceiverOkay && invalidTargetOkay && mismatch && bigOkay && detachedOkay;
"#;

const BIGINT_TYPED_ARRAY_CALLBACK_SOURCE: &str = r#"
function verify(TA, input, expected) {
  var array = new TA(input);
  var receiverOkay = true;
  var types = "";
  var thisArg = { marker: 17 };
  var forEachResult = array.forEach(function(value, index, receiver) {
    receiverOkay = receiverOkay && receiver === array && this === thisArg && value === expected[index];
    types += typeof value;
  }, thisArg);

  var leftIndexes = "";
  var leftAccumulator = array.reduce(function(accumulator, value, index, receiver) {
    "use strict";
    receiverOkay = receiverOkay && receiver === array && this === undefined &&
      typeof accumulator === "bigint" && typeof value === "bigint" && value === expected[index];
    leftIndexes += index;
    return value;
  });

  var rightIndexes = "";
  var rightAccumulator = array.reduceRight(function(accumulator, value, index, receiver) {
    "use strict";
    receiverOkay = receiverOkay && receiver === array && this === undefined &&
      accumulator === thisArg && typeof value === "bigint" && value === expected[index];
    rightIndexes += index;
    return accumulator;
  }, thisArg);

  return receiverOkay && forEachResult === undefined &&
    types === "bigintbigintbigint" && leftIndexes === "12" &&
    leftAccumulator === expected[2] && rightIndexes === "210" && rightAccumulator === thisArg;
}

var signed = verify(
  BigInt64Array,
  [9223372036854775807n, 9223372036854775808n, 18446744073709551615n],
  [9223372036854775807n, -9223372036854775808n, -1n]
);
var unsigned = verify(
  BigUint64Array,
  [-1n, 9223372036854775808n, 18446744073709551615n],
  [18446744073709551615n, 9223372036854775808n, 18446744073709551615n]
);
var explicitUndefinedCalls = 0;
var explicitUndefined = new BigInt64Array([7n]).reduce(function(accumulator, value) {
  explicitUndefinedCalls++;
  return accumulator === undefined && typeof value === "bigint" ? value : 99n;
}, undefined);
var live = new BigInt64Array([1n, 2n]);
var liveOkay = true;
live.forEach(function(value, index, receiver) {
  if (index === 0) liveOkay = Reflect.set(receiver, 1, 9n);
  else liveOkay = typeof value === "bigint" && value === 9n;
});
var detached = new BigUint64Array([5n, 6n]);
var detachedOkay = true;
detached.reduce(function(accumulator, value, index, receiver) {
  if (index === 0) $262.detachArrayBuffer(receiver.buffer);
  else detachedOkay = value === undefined;
  return accumulator;
}, this);
signed && unsigned && explicitUndefined === 7n && explicitUndefinedCalls === 1 &&
  liveOkay && detachedOkay;
"#;

#[test]
fn typed_array_callbacks_work_for_every_dispatch_batch() {
    assert_typed_array_callbacks::<1>(false);
    assert_typed_array_callbacks::<2>(false);
    assert_typed_array_callbacks::<4>(false);
    assert_typed_array_callbacks::<8>(false);
    assert_typed_array_callbacks::<16>(false);
}

#[test]
fn typed_array_callback_state_survives_forced_major_collection() {
    assert_typed_array_callbacks::<8>(true);
}

#[test]
fn typed_array_callback_loop_does_not_grow_rust_stack() {
    let module = compile_typed_array_callback_fixture(TYPED_ARRAY_CALLBACK_LONG_SOURCE);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_000_000,
                quantum: 4_000_000,
            },
        )
        .expect("long TypedArray callback fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long callback loop returned {outcome:?}"
    );
}

#[test]
fn bigint_typed_array_callbacks_preserve_primitive_values() {
    assert_bigint_typed_array_callbacks::<1>(false);
    assert_bigint_typed_array_callbacks::<2>(false);
    assert_bigint_typed_array_callbacks::<4>(false);
    assert_bigint_typed_array_callbacks::<8>(false);
    assert_bigint_typed_array_callbacks::<16>(false);
}

#[test]
fn bigint_typed_array_callback_values_survive_forced_major_collection() {
    assert_bigint_typed_array_callbacks::<1>(true);
    assert_bigint_typed_array_callbacks::<2>(true);
    assert_bigint_typed_array_callbacks::<4>(true);
    assert_bigint_typed_array_callbacks::<8>(true);
    assert_bigint_typed_array_callbacks::<16>(true);
}

#[test]
fn typed_array_reflect_set_obeys_integer_indexed_receiver_rules() {
    assert_typed_array_reflect_set::<1>(false);
    assert_typed_array_reflect_set::<2>(false);
    assert_typed_array_reflect_set::<4>(false);
    assert_typed_array_reflect_set::<8>(false);
    assert_typed_array_reflect_set::<16>(false);
    assert_typed_array_reflect_set::<8>(true);
}

/// Executes all callback modes, metadata, branding, and rooting under one policy.
fn assert_typed_array_callbacks<const N: usize>(forced_major: bool) {
    let source = if forced_major {
        TYPED_ARRAY_CALLBACK_GC_SOURCE
    } else {
        TYPED_ARRAY_CALLBACK_SOURCE
    };
    let module = compile_typed_array_callback_fixture(source);
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
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("TypedArray callback fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Exercises Number-free BigInt callback transport under one dispatch and GC policy.
fn assert_bigint_typed_array_callbacks<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_callback_fixture(BIGINT_TYPED_ARRAY_CALLBACK_SOURCE);
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
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("BigInt TypedArray callback fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Exercises integer-indexed Reflect.set, alternate receivers, detach, and ContentType checks.
fn assert_typed_array_reflect_set<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_callback_fixture(TYPED_ARRAY_REFLECT_SET_SOURCE);
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
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("TypedArray Reflect.set fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
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
    _kind: crate::DynamicFunctionKind,
    _source: crate::DynamicFunctionSource,
) -> Result<Value, ExecutionError> {
    Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
}

/// Compiles the shared callback-family fixture independently of VM scheduling policy.
fn compile_typed_array_callback_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_429),
                SourceName::new("typed-array-callback-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray callback fixture compiles")
}
