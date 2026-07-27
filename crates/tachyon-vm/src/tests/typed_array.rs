use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_SOURCE: &str = r#"
var i8 = new Int8Array(2);
var u8 = new Uint8Array(2);
var clamped = new Uint8ClampedArray(2);
var i16 = new Int16Array(1);
var u16 = new Uint16Array(1);
var i32 = new Int32Array(1);
var u32 = new Uint32Array(1);
var f32 = new Float32Array(1);
var f64 = new Float64Array(1);
i8[0] = 257;
u8[0] = -1;
clamped[0] = 2.5;
clamped[1] = 3.5;
i16[0] = -2;
u16[0] = -1;
i32[0] = 4294967295;
u32[0] = -1;
f32[0] = 1.5;
f64[0] = -2.25;

var buffer = new ArrayBuffer(16);
var view = new Uint32Array(buffer, 4, 2);
view[0] = 0x12345678;
var bytes = new Uint8Array(buffer);
var descriptor = Object.getOwnPropertyDescriptor(view, "0");
var keys = Reflect.ownKeys(view);
var rejected = Reflect.defineProperty(view, "0", { configurable: false });
view["1.0"] = 9;

var iterableSource = [
  { valueOf: function() { iterableSource[1] = 9; return 1; } },
  2
];
var fromIterable = new Uint8Array(iterableSource);
var arrayLikeSource = {
  length: 2,
  0: { valueOf: function() { arrayLikeSource[1] = 9; return 3; } },
  1: 4
};
var fromArrayLike = new Uint8Array(arrayLikeSource);
var copiedSameKind = new Uint8Array(fromIterable);
var copiedOtherKind = new Int16Array(fromIterable);
fromIterable[0] = 7;
class DerivedUint8Array extends Uint8Array {}
var derived = new DerivedUint8Array([5, 6]);
var descriptorTarget = new Uint8Array(2);
var minusZeroDefined = Reflect.defineProperty(descriptorTarget, "-0", {
  value: 42, configurable: false, enumerable: true, writable: true
});
var fractionalDefined = Reflect.defineProperty(descriptorTarget, "0.1", {
  value: 42, configurable: false, enumerable: true, writable: true
});
var accessorDefined = Reflect.defineProperty(descriptorTarget, "0", {
  get: function() { return 42; }, enumerable: true
});
var nonEnumerableDefined = Reflect.defineProperty(descriptorTarget, "0", {
  value: 42, configurable: false, enumerable: false, writable: true
});
var nonWritableDefined = Reflect.defineProperty(descriptorTarget, "0", {
  value: 42, configurable: false, enumerable: true, writable: false
});
var numberTypedArrayConstructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var descriptorHarnessOkay = true;
function descriptorPassthrough(TA, value) { return value; }
for (var descriptorIndex = 0; descriptorIndex < numberTypedArrayConstructors.length; descriptorIndex++) {
  var descriptorConstructor = numberTypedArrayConstructors[descriptorIndex];
  var descriptorArgument = descriptorPassthrough.bind(undefined, descriptorConstructor);
  var descriptorSample = new descriptorConstructor(descriptorArgument(2));
  var descriptorResult = Reflect.defineProperty(descriptorSample, "-0", {
      value: 42, configurable: false, enumerable: true, writable: true
    });
  var descriptorIterationOkay = descriptorResult === false &&
    descriptorSample[0] === 0 && descriptorSample["-0"] === undefined;
  descriptorHarnessOkay = descriptorHarnessOkay && descriptorIterationOkay;
}

i8[0] === 1 && u8[0] === 255 &&
clamped[0] === 2 && clamped[1] === 4 &&
i16[0] === -2 && u16[0] === 65535 &&
i32[0] === -1 && u32[0] === 4294967295 &&
f32[0] === 1.5 && f64[0] === -2.25 &&
view.buffer === buffer && view.byteOffset === 4 && view.byteLength === 8 && view.length === 2 &&
bytes[4] === 0x78 && bytes[5] === 0x56 && bytes[6] === 0x34 && bytes[7] === 0x12 &&
ArrayBuffer.isView(view) && Object.getPrototypeOf(view) === Uint32Array.prototype &&
Object.getPrototypeOf(Uint32Array.prototype) === Object.getPrototypeOf(Int8Array.prototype) &&
Object.getPrototypeOf(Uint32Array.prototype) === Object.getPrototypeOf(Uint32Array).prototype &&
Object.prototype.toString.call(view) === "[object Uint32Array]" &&
Uint32Array.name === "Uint32Array" && Uint32Array.length === 3 &&
Uint32Array.BYTES_PER_ELEMENT === 4 && Uint32Array.prototype.BYTES_PER_ELEMENT === 4 &&
descriptor.value === 0x12345678 && descriptor.writable && descriptor.enumerable && descriptor.configurable &&
keys[0] === "0" && keys[1] === "1" && delete view[0] === false &&
rejected === false && view["1.0"] === 9 && view[-0] === view[0] &&
fromIterable.length === 2 && fromIterable[1] === 2 &&
fromArrayLike.length === 2 && fromArrayLike[0] === 3 && fromArrayLike[1] === 9 &&
copiedSameKind.length === 2 && copiedSameKind[0] === 1 && copiedSameKind[1] === 2 &&
copiedOtherKind.length === 2 && copiedOtherKind[0] === 1 && copiedOtherKind[1] === 2 &&
derived.length === 2 && derived[0] === 5 && derived[1] === 6 &&
Object.getPrototypeOf(derived) === DerivedUint8Array.prototype &&
minusZeroDefined === false && fractionalDefined === false &&
accessorDefined === false && nonEnumerableDefined === false && nonWritableDefined === false &&
descriptorTarget[0] === 0 && descriptorTarget["-0"] === undefined &&
descriptorHarnessOkay;
"#;

const TYPED_ARRAY_ITERATOR_SOURCE: &str = r#"
var array = new Uint8Array([3, 7, 11]);
typeof array.keys === "function" && typeof array.values === "function" &&
  typeof array.entries === "function" && array.keys() !== undefined &&
  array.values() !== undefined && array.entries() !== undefined;
"#;

const TYPED_ARRAY_SORT_SOURCE: &str = r#"
var signed = new Int8Array([3, -1, 2, -8]);
var unsigned = new Uint32Array([9, 1, 4294967295, 4]);
var floats = new Float64Array([NaN, 0, -0, 4.5, -2]);
var bigints = new BigInt64Array([3n, -1n, 2n, -8n]);
var identity = signed.sort() === signed;
unsigned.sort();
floats.sort();
bigints.sort();
var compareThrows = false;
try { signed.sort(1); } catch (error) { compareThrows = error instanceof TypeError; }
identity && signed.join(",") === "-8,-1,2,3" &&
  unsigned.join(",") === "1,4,9,4294967295" &&
  floats[0] === -2 && 1 / floats[1] === -Infinity &&
  1 / floats[2] === Infinity && floats[3] === 4.5 && Number.isNaN(floats[4]) &&
  bigints[0] === -8n && bigints[1] === -1n && bigints[2] === 2n && bigints[3] === 3n &&
  compareThrows && Int8Array.prototype.sort.length === 1;
"#;

const TYPED_ARRAY_CALLABLE_SORT_SOURCE: &str = r#"
var calls = 0;
var parity = new Int16Array([7, 2, 5, 4, 3, 6, 1]);
var identity = parity.sort(function(left, right) {
  calls++;
  return {
    valueOf: function() {
      return (left % 2) - (right % 2);
    }
  };
}) === parity;

var descending = new Float64Array([-0, 3, 1, 2]);
descending.sort(function(left, right) {
  return right - left;
});
var nanEqual = new Uint8Array([3, 2, 1]);
nanEqual.sort(function() { return NaN; });

var marker = { marker: true };
var abruptIdentity = false;
try {
  new Uint8Array([3, 2, 1]).sort(function() { throw marker; });
} catch (error) {
  abruptIdentity = error === marker;
}
var bigints = new BigInt64Array([3n, -2n, 1n]);
bigints.sort(function() { return 0; });

var detached = new Uint8Array([4, 3, 2, 1]);
var detachedBuffer = detached.buffer;
var detachCalls = 0;
var detachedIdentity = detached.sort(function(left, right) {
  detachCalls++;
  if (detachCalls === 1) detachedBuffer.transfer();
  return left - right;
}) === detached;

identity && calls > 0 && parity.join(",") === "2,4,6,7,5,3,1" &&
  descending[0] === 3 && descending[1] === 2 && descending[2] === 1 &&
  1 / descending[3] === -Infinity && nanEqual.join(",") === "3,2,1" &&
  abruptIdentity && bigints.join(",") === "3,-2,1" &&
  detachedIdentity && detachCalls > 0 && detached.length === 0;
"#;

const LARGE_TYPED_ARRAY_SOURCE: &str = r#"
var source = new Array(10000).fill(7);
var copied = Array.from(source);
var iterator = source[Symbol.iterator];
source[Symbol.iterator] = undefined;
var arrayLikeResult = new Uint8Array(source);
source[Symbol.iterator] = iterator;
var iterableResult = new Uint8Array(source);
copied.length === 10000 && copied[9999] === 7 &&
arrayLikeResult.length === 10000 && arrayLikeResult[9999] === 7 &&
iterableResult.length === 10000 && iterableResult[0] === 7 && iterableResult[9999] === 7;
"#;

const BIGINT_TYPED_ARRAY_SOURCE: &str = r#"
function throwsTypeError(callback) {
  try {
    callback();
    return false;
  } catch (error) {
    return error instanceof TypeError;
  }
}

var signed = new BigInt64Array(5);
signed[0] = -1n;
signed[1] = 9223372036854775807n;
signed[2] = 9223372036854775808n;
signed[3] = -9223372036854775808n;
signed[4] = 18446744073709551616n;

var unsigned = new BigUint64Array([-1n, 18446744073709551616n, 9223372036854775808n]);
var signedCopy = new BigInt64Array(unsigned);
var unsignedCopy = new BigUint64Array(signed);
var bytes = new Uint8Array(signed.buffer);
var explicitBuffer = new ArrayBuffer(24);
var explicitView = new BigUint64Array(explicitBuffer, 8, 2);
explicitView[1] = 42n;

var constructorNumberMismatch = throwsTypeError(function() {
  new BigInt64Array([1]);
});
var constructorBigIntMismatch = throwsTypeError(function() {
  new Int32Array([1n]);
});
var typedSourceNumberMismatch = throwsTypeError(function() {
  new BigUint64Array(new Uint32Array(1));
});
var typedSourceBigIntMismatch = throwsTypeError(function() {
  new Uint32Array(new BigInt64Array(1));
});
var indexedNumberMismatch = throwsTypeError(function() {
  signed[0] = 1;
});
var numberIndexedBigIntMismatch = throwsTypeError(function() {
  new Uint32Array(1)[0] = 1n;
});

signed[0] === -1n &&
signed[1] === 9223372036854775807n &&
signed[2] === -9223372036854775808n &&
signed[3] === -9223372036854775808n &&
signed[4] === 0n &&
unsigned[0] === 18446744073709551615n &&
unsigned[1] === 0n &&
unsigned[2] === 9223372036854775808n &&
signedCopy[0] === -1n && signedCopy[1] === 0n &&
unsignedCopy[0] === 18446744073709551615n &&
explicitView.buffer === explicitBuffer && explicitView.byteOffset === 8 &&
explicitView.byteLength === 16 && explicitView.length === 2 && explicitView[1] === 42n &&
bytes[0] === 255 && bytes[1] === 255 && bytes[2] === 255 && bytes[3] === 255 &&
bytes[4] === 255 && bytes[5] === 255 && bytes[6] === 255 && bytes[7] === 255 &&
BigInt64Array.name === "BigInt64Array" && BigInt64Array.length === 3 &&
BigInt64Array.BYTES_PER_ELEMENT === 8 && BigInt64Array.prototype.BYTES_PER_ELEMENT === 8 &&
BigUint64Array.name === "BigUint64Array" && BigUint64Array.length === 3 &&
Object.prototype.toString.call(signed) === "[object BigInt64Array]" &&
Object.prototype.toString.call(unsigned) === "[object BigUint64Array]" &&
constructorNumberMismatch && constructorBigIntMismatch &&
typedSourceNumberMismatch && typedSourceBigIntMismatch &&
indexedNumberMismatch && numberIndexedBigIntMismatch;
"#;

#[test]
fn fixed_number_typed_arrays_work_for_every_dispatch_batch() {
    assert_typed_array_source::<1>(false);
    assert_typed_array_source::<2>(false);
    assert_typed_array_source::<4>(false);
    assert_typed_array_source::<8>(false);
    assert_typed_array_source::<16>(false);
}

#[test]
fn typed_array_edges_survive_forced_major_collection() {
    assert_typed_array_source::<8>(true);
}

#[test]
fn bigint_typed_arrays_work_for_every_dispatch_batch() {
    assert_bigint_typed_array_source::<1>(false);
    assert_bigint_typed_array_source::<2>(false);
    assert_bigint_typed_array_source::<4>(false);
    assert_bigint_typed_array_source::<8>(false);
    assert_bigint_typed_array_source::<16>(false);
}

#[test]
fn bigint_typed_array_edges_survive_forced_major_collection() {
    assert_bigint_typed_array_source::<1>(true);
    assert_bigint_typed_array_source::<2>(true);
    assert_bigint_typed_array_source::<4>(true);
    assert_bigint_typed_array_source::<8>(true);
    assert_bigint_typed_array_source::<16>(true);
}

#[test]
/// Uses larger atom and shape quotas because the source owns 10,000 indexed properties.
fn intrinsic_iterable_collection_does_not_grow_the_rust_stack() {
    let module = compile_typed_array_source(LARGE_TYPED_ARRAY_SOURCE, 7_422);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(32 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 32_768).with_max_shapes(32_768),
    ))
    .expect("large TypedArray iterable isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 1_000_000,
                quantum: 1_000_000,
            },
        )
        .expect("large TypedArray iterable executes without Rust recursion");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(511)),
        "large TypedArray iterable returned {outcome:?}"
    );
}

#[test]
fn typed_array_iterators_work_for_every_dispatch_batch() {
    assert_typed_array_iterators::<1>(false);
    assert_typed_array_iterators::<2>(false);
    assert_typed_array_iterators::<4>(false);
    assert_typed_array_iterators::<8>(false);
    assert_typed_array_iterators::<16>(false);
}

#[test]
fn typed_array_iterators_survive_forced_major_collection() {
    assert_typed_array_iterators::<8>(true);
}

#[test]
fn typed_array_default_sort_works_for_every_dispatch_batch() {
    assert_typed_array_sort::<1>(false);
    assert_typed_array_sort::<2>(false);
    assert_typed_array_sort::<4>(false);
    assert_typed_array_sort::<8>(false);
    assert_typed_array_sort::<16>(false);
}

#[test]
fn typed_array_default_sort_survives_forced_major_collection() {
    assert_typed_array_sort::<8>(true);
}

#[test]
fn typed_array_callable_sort_works_for_every_dispatch_batch() {
    assert_typed_array_callable_sort::<1>(false);
    assert_typed_array_callable_sort::<2>(false);
    assert_typed_array_callable_sort::<4>(false);
    assert_typed_array_callable_sort::<8>(false);
    assert_typed_array_callable_sort::<16>(false);
}

#[test]
fn typed_array_callable_sort_survives_forced_major_collection() {
    assert_typed_array_callable_sort::<1>(true);
    assert_typed_array_callable_sort::<2>(true);
    assert_typed_array_callable_sort::<4>(true);
    assert_typed_array_callable_sort::<8>(true);
    assert_typed_array_callable_sort::<16>(true);
}

/// Executes default numeric ordering under one dispatch and collection policy.
fn assert_typed_array_sort<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_source(TYPED_ARRAY_SORT_SOURCE, 7_500 + N as u32);
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
        .expect("TypedArray sort fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes resumable comparator calls and detach handling under one VM policy.
fn assert_typed_array_callable_sort<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_source(TYPED_ARRAY_CALLABLE_SORT_SOURCE, 7_550 + N as u32);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(14 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("callable TypedArray sort isolate initializes");
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
        .expect("callable TypedArray sort fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes the three shared iterator projections with both dispatch and GC policies.
fn assert_typed_array_iterators<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_source(TYPED_ARRAY_ITERATOR_SOURCE, 7_460 + N as u32);
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
        .expect("TypedArray iterator fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes construction, integer-indexed MOP, conversion, and metadata checks under one policy.
fn assert_typed_array_source<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_fixture();
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
        .expect("TypedArray fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes BigInt content conversion, storage, and mismatch checks under one policy.
fn assert_bigint_typed_array_source<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_source(
        BIGINT_TYPED_ARRAY_SOURCE,
        7_450 + N as u32 + u32::from(forced_major) * 32,
    );
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
        .expect("BigInt TypedArray fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the shared fixture independently of dispatch and collection policy.
fn compile_typed_array_fixture() -> CompiledModule {
    compile_typed_array_source(TYPED_ARRAY_SOURCE, 7_421)
}

/// Compiles one TypedArray fixture independently of dispatch and collection policy.
fn compile_typed_array_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("typed-array-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray fixture compiles")
}
