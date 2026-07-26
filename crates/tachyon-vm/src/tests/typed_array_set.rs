use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_SET_SOURCE: &str = r#"
function verify(TA) {
  var target = new TA(5);
  var source = new TA([1, 2, 3]);
  var result = target.set(source, 1);
  var arrayLike = { length: 2, 0: 7, 1: 8 };
  target.set(arrayLike, 0);
  return result === undefined && target[0] === 7 && target[1] === 8 &&
    target[2] === 2 && target[3] === 3 && target[4] === 0;
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var constructorsOkay = true;
for (var i = 0; i < constructors.length; i++) constructorsOkay = constructorsOkay && verify(constructors[i]);

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "set");
var metadataOkay = property.value.name === "set" && property.value.length === 1 &&
  property.writable === true && property.enumerable === false && property.configurable === true;
var rejected = false;
try { property.value.call({}, [1]); } catch (error) { rejected = error instanceof TypeError; }
var emptyRangeRejected = false;
try { new Uint8Array(1).set([], 2); } catch (error) { emptyRangeRejected = error instanceof RangeError; }

var forwardBuffer = new ArrayBuffer(8);
var forward = new Uint8Array(forwardBuffer);
for (var f = 0; f < 8; f++) forward[f] = f + 1;
new Uint8Array(forwardBuffer, 2, 6).set(new Uint8Array(forwardBuffer, 0, 6));
var forwardOkay = forward[0] === 1 && forward[1] === 2 && forward[2] === 1 &&
  forward[3] === 2 && forward[4] === 3 && forward[5] === 4 && forward[6] === 5 && forward[7] === 6;

var backwardBuffer = new ArrayBuffer(8);
var backward = new Uint8Array(backwardBuffer);
for (var b = 0; b < 8; b++) backward[b] = b + 1;
new Uint8Array(backwardBuffer, 0, 6).set(new Uint8Array(backwardBuffer, 2, 6));
var backwardOkay = backward[0] === 3 && backward[1] === 4 && backward[2] === 5 &&
  backward[3] === 6 && backward[4] === 7 && backward[5] === 8 && backward[6] === 7 && backward[7] === 8;

var crossBuffer = new ArrayBuffer(8);
var crossSource = new Uint8Array(crossBuffer, 0, 4);
crossSource[0] = 1; crossSource[1] = 2; crossSource[2] = 3; crossSource[3] = 4;
var crossTarget = new Uint16Array(crossBuffer);
crossTarget.set(crossSource);
var crossOkay = crossTarget[0] === 1 && crossTarget[1] === 2 &&
  crossTarget[2] === 3 && crossTarget[3] === 4;

var offsetBuffer = new ArrayBuffer(10);
var offsetWhole = new Uint8Array(offsetBuffer);
var offsetView = new Uint8Array(offsetBuffer, 2, 6);
offsetView.set([9, 8], 2);
var offsetOkay = offsetWhole[0] === 0 && offsetWhole[1] === 0 && offsetWhole[4] === 9 &&
  offsetWhole[5] === 8 && offsetWhole[8] === 0 && offsetWhole[9] === 0;

var numericTarget = new Float64Array(4);
numericTarget.set(new Int16Array([-2, 300, 0, 7]));
var crossKindOkay = numericTarget[0] === -2 && numericTarget[1] === 300 &&
  numericTarget[2] === 0 && numericTarget[3] === 7;
var clamped = new Uint8ClampedArray(5);
clamped.set([-1, 0.5, 1.5, 254.5, 300]);
var clampedOkay = clamped[0] === 0 && clamped[1] === 0 && clamped[2] === 2 &&
  clamped[3] === 254 && clamped[4] === 255;

var bitsSourceBuffer = new ArrayBuffer(16);
var bitsSourceWords = new Uint32Array(bitsSourceBuffer);
bitsSourceWords[0] = 0x12345678; bitsSourceWords[1] = 0x7ff80000;
bitsSourceWords[2] = 0x87654321; bitsSourceWords[3] = 0x7ff80000;
var bitsTargetBuffer = new ArrayBuffer(16);
new Float64Array(bitsTargetBuffer).set(new Float64Array(bitsSourceBuffer));
var bitsTargetWords = new Uint32Array(bitsTargetBuffer);
var bitsOkay = bitsTargetWords[0] === 0x12345678 && bitsTargetWords[1] === 0x7ff80000 &&
  bitsTargetWords[2] === 0x87654321 && bitsTargetWords[3] === 0x7ff80000;

constructorsOkay && metadataOkay && rejected && emptyRangeRejected && forwardOkay && backwardOkay && crossOkay &&
  offsetOkay && crossKindOkay && clampedOkay && bitsOkay;
"#;

const TYPED_ARRAY_SET_OBSERVABLE_SOURCE: &str = r#"
var target = new Uint8Array(4);
var log = "";
var source = {};
Object.defineProperty(source, "length", { get: function() { log += "l"; return 2; } });
Object.defineProperty(source, "0", { get: function() {
  log += "a";
  return { valueOf: function() { log += "A"; return 11; } };
} });
Object.defineProperty(source, "1", { get: function() { log += "b"; return 12; } });
target.set(source, { valueOf: function() { log += "o"; return 1; } });

var abrupt = {};
var abruptSource = { length: 2, 0: 21 };
Object.defineProperty(abruptSource, "1", { get: function() { throw abrupt; } });
var abruptIdentity = false;
try { target.set(abruptSource); } catch (error) { abruptIdentity = error === abrupt; }

var detached = new Uint8Array([1, 2, 3]);
var laterGet = false;
var detachSource = { length: 3, 0: 31 };
Object.defineProperty(detachSource, "1", { get: function() {
  $262.detachArrayBuffer(detached.buffer);
  return 32;
} });
Object.defineProperty(detachSource, "2", { get: function() { laterGet = true; return 33; } });
var detachThrew = false;
try { detached.set(detachSource); } catch (error) { detachThrew = true; }

var detachedOnOffset = new Uint8Array(1);
var lengthRead = false;
var lateSource = {};
Object.defineProperty(lateSource, "length", { get: function() { lengthRead = true; return 0; } });
var offsetThrew = false;
try {
  detachedOnOffset.set(lateSource, { valueOf: function() {
    $262.detachArrayBuffer(detachedOnOffset.buffer);
    return 0;
  } });
} catch (error) { offsetThrew = error instanceof TypeError; }

var detachedBeforeCall = new Uint8Array(1);
$262.detachArrayBuffer(detachedBeforeCall.buffer);
var detachedOffsetCalls = 0;
var detachedLengthRead = false;
var detachedSource = {};
Object.defineProperty(detachedSource, "length", { get: function() {
  detachedLengthRead = true;
  return 0;
} });
var detachedBeforeCallThrew = false;
try {
  detachedBeforeCall.set(detachedSource, { valueOf: function() {
    detachedOffsetCalls += 1;
    return 0;
  } });
} catch (error) { detachedBeforeCallThrew = error instanceof TypeError; }

log === "olaAb" && target[0] === 21 && target[1] === 11 && target[2] === 12 &&
  abruptIdentity && laterGet && !detachThrew && detached.length === 0 &&
  offsetThrew && !lengthRead && detachedBeforeCallThrew &&
  detachedOffsetCalls === 1 && !detachedLengthRead;
"#;

const TYPED_ARRAY_SET_LONG_SOURCE: &str = r#"
var length = 20000;
var source = [];
for (var i = 0; i < length; i++) source[i] = i;
var target = new Uint32Array(length);
var returned = target.set(source);
returned === undefined && target[0] === 0 && target[1] === 1 &&
  target[length - 2] === length - 2 && target[length - 1] === length - 1;
"#;

#[test]
fn typed_array_set_works_for_every_dispatch_batch() {
    assert_typed_array_set::<1>(TYPED_ARRAY_SET_SOURCE, false);
    assert_typed_array_set::<2>(TYPED_ARRAY_SET_SOURCE, false);
    assert_typed_array_set::<4>(TYPED_ARRAY_SET_SOURCE, false);
    assert_typed_array_set::<8>(TYPED_ARRAY_SET_SOURCE, false);
    assert_typed_array_set::<16>(TYPED_ARRAY_SET_SOURCE, false);
}

#[test]
fn typed_array_set_observable_state_survives_forced_major_collection() {
    assert_typed_array_set::<8>(TYPED_ARRAY_SET_OBSERVABLE_SOURCE, true);
}

#[test]
/// Uses a larger atom quota because array-like indexed Get materializes property-key atoms.
fn typed_array_set_large_array_like_does_not_grow_rust_stack() {
    let module = compile_typed_array_set_fixture(TYPED_ARRAY_SET_LONG_SOURCE);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(32 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 32_768).with_max_shapes(32_768),
    ))
    .expect("large-atom TypedArray set isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_000_000,
                quantum: 4_000_000,
            },
        )
        .expect("long TypedArray set executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long TypedArray set returned {outcome:?}"
    );
}

/// Executes one set fixture under the selected dispatch and collection policy.
fn assert_typed_array_set<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_set_fixture(source);
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
                fuel: 4_000_000,
                quantum: 4_000_000,
            },
        )
        .expect("TypedArray set fixture executes");
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
) -> Result<Value, ExecutionError> {
    Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
}

/// Compiles one set fixture independently of VM scheduling policy.
fn compile_typed_array_set_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_434),
                SourceName::new("typed-array-set-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray set fixture compiles")
}
