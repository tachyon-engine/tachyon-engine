use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

const TYPED_ARRAY_WITH_SOURCE: &str = r#"
function verify(TA) {
  var source = new TA([1, 2, 3]);
  source.constructor = { get [Symbol.species]() { throw new Error("species"); } };
  var result = source.with(-1, 9);
  return result !== source && Object.getPrototypeOf(result) === TA.prototype &&
    source[2] === 3 && result[0] === 1 && result[1] === 2 && result[2] === 9;
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var valuesOkay = true;
for (var i = 0; i < constructors.length; i++) valuesOkay = valuesOkay && verify(constructors[i]);

var signed = new BigInt64Array([1n, 2n]).with(0, 18446744073709551615n);
var unsigned = new BigUint64Array([1n, 2n]).with(1, "18446744073709551615");
var bigintOkay = signed[0] === -1n && signed[1] === 2n &&
  unsigned[0] === 1n && unsigned[1] === 18446744073709551615n;

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "with");
var metadataOkay = property.value.name === "with" && property.value.length === 2 &&
  property.writable === true && property.enumerable === false && property.configurable === true;

var order = "";
var ordered = new Uint8Array([1, 2, 3]);
var orderedResult = ordered.with(
  { valueOf: function() { order += "i"; return 1.9; } },
  { valueOf: function() { order += "v"; ordered[0] = 7; return 8; } }
);
var abrupt = {};
var abruptIdentity = false;
try {
  ordered.with(99, { valueOf: function() { throw abrupt; } });
} catch (error) { abruptIdentity = error === abrupt; }
var rangeAfterValue = false;
var converted = false;
try {
  ordered.with(99, { valueOf: function() { converted = true; return 1; } });
} catch (error) { rangeAfterValue = error instanceof RangeError; }

valuesOkay && bigintOkay && metadataOkay && order === "iv" &&
  orderedResult[0] === 7 && orderedResult[1] === 8 && orderedResult[2] === 3 &&
  abruptIdentity && converted && rangeAfterValue;
"#;

const TYPED_ARRAY_WITH_GC_SOURCE: &str = r#"
var order = "";
var array = new Uint8Array([1, 2, 3]);
var detachedRange = false;
try {
  array.with(
    { valueOf: function() { order += "i"; $262.detachArrayBuffer(array.buffer); return 0; } },
    { valueOf: function() { order += "v"; return 9; } }
  );
} catch (error) { detachedRange = error instanceof RangeError; }

var big = new BigInt64Array([1n, 2n]);
var bigResult = big.with(
  { valueOf: function() { return 1; } },
  { valueOf: function() { return 9223372036854775808n; } }
);
order === "iv" && detachedRange && bigResult[0] === 1n &&
  bigResult[1] === -9223372036854775808n;
"#;

const TYPED_ARRAY_WITH_CROSS_REALM_SOURCE: &str = r#"
var source = new foreignUint16Array([10, 20, 30]);
source.constructor = foreignUint16Array;
var method = Object.getPrototypeOf(Uint16Array.prototype).with;
var result = method.call(source, 1, 99);
Object.getPrototypeOf(result) === Uint16Array.prototype &&
  result[0] === 10 && result[1] === 99 && result[2] === 30;
"#;

const TYPED_ARRAY_WITH_RAB_SOURCE: &str = r#"
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray,
  BigInt64Array, BigUint64Array
];
var okay = true;
for (var i = 0; i < constructors.length; i++) {
  var TA = constructors[i];
  var width = TA.BYTES_PER_ELEMENT;
  var rab = new ArrayBuffer(width * 3, { maxByteLength: width * 6 });
  var source = new TA(rab);
  var replacement = i < 9 ? 7 : 7n;
  var result = source.with(1, replacement);
  okay = okay && source.length === 3 && result.length === 3 && result[1] === replacement;
}
okay;
"#;

const TYPED_ARRAY_WITH_SOURCE_FACTORIES: &str = r#"
function copy(dest, source) {
  var out = new Uint8Array(dest);
  var input = new Uint8Array(source);
  for (var i = 0; i < input.length; i++) out[i] = input[i];
  return dest;
}
function args(TA) {
  var values = [0, 1, 2];
  var fixed = new TA(values).buffer;
  var bytes = fixed.byteLength;
  var resizable = copy(new ArrayBuffer(bytes, { maxByteLength: bytes * 2 }), fixed);
  var grown = new ArrayBuffer(Math.floor(bytes / 2), { maxByteLength: bytes });
  grown.resize(bytes);
  copy(grown, fixed);
  var shrunk = copy(new ArrayBuffer(bytes * 2, { maxByteLength: bytes * 2 }), fixed);
  shrunk.resize(bytes);
  var arrayLike = { 0: 0, 1: 1, 2: 2, length: 3 };
  var iterable = {};
  iterable[Symbol.iterator] = function() { return values[Symbol.iterator](); };
  return [values, values.slice(), arrayLike, iterable, fixed, resizable, grown, shrunk];
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var okay = true;
for (var c = 0; c < constructors.length; c++) {
  var TA = constructors[c];
  var sources = args(TA);
  for (var f = 0; f < sources.length; f++) {
    var source = new TA(sources[f]);
    var result = source.with(1, 4);
    okay = okay && result[0] === 0 && result[1] === 4 && result[2] === 2;
  }
}
okay;
"#;

#[test]
fn typed_array_with_works_for_every_dispatch_batch() {
    assert_typed_array_with::<1>(TYPED_ARRAY_WITH_SOURCE, false);
    assert_typed_array_with::<2>(TYPED_ARRAY_WITH_SOURCE, false);
    assert_typed_array_with::<4>(TYPED_ARRAY_WITH_SOURCE, false);
    assert_typed_array_with::<8>(TYPED_ARRAY_WITH_SOURCE, false);
    assert_typed_array_with::<16>(TYPED_ARRAY_WITH_SOURCE, false);
}

#[test]
fn typed_array_with_state_survives_forced_major_collection() {
    assert_typed_array_with::<8>(TYPED_ARRAY_WITH_GC_SOURCE, true);
}

#[test]
fn typed_array_with_uses_the_active_realm_same_kind_intrinsic() {
    let module = compile_typed_array_with_fixture(TYPED_ARRAY_WITH_CROSS_REALM_SOURCE);
    // Two complete Realms include SharedArrayBuffer and the default Atomics namespace.
    let mut isolate = test_isolate_with_heap_spans(11);
    let (_, child_global) = isolate.create_realm().expect("child Realm initializes");
    let constructor_atom = isolate.intern_intrinsic_name(b"Uint16Array").unwrap();
    let foreign_constructor = isolate
        .get_data_property(child_global, constructor_atom)
        .unwrap()
        .expect("child Realm publishes Uint16Array");
    let foreign_atom = isolate
        .intern_intrinsic_name(b"foreignUint16Array")
        .unwrap();
    let global = isolate
        .realm
        .global_object
        .expect("main global initializes");
    isolate
        .set_own_data_property(global, foreign_atom, foreign_constructor)
        .unwrap();
    isolate
        .realm
        .set(foreign_atom, foreign_constructor)
        .unwrap();
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("cross-Realm TypedArray with executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "cross-Realm TypedArray with returned {outcome:?}"
    );
}

#[test]
fn typed_array_with_copies_length_tracking_rab_sources() {
    assert_typed_array_with::<1>(TYPED_ARRAY_WITH_RAB_SOURCE, false);
    assert_typed_array_with::<2>(TYPED_ARRAY_WITH_RAB_SOURCE, false);
    assert_typed_array_with::<4>(TYPED_ARRAY_WITH_RAB_SOURCE, false);
    assert_typed_array_with::<8>(TYPED_ARRAY_WITH_RAB_SOURCE, false);
    assert_typed_array_with::<16>(TYPED_ARRAY_WITH_RAB_SOURCE, false);
    assert_typed_array_with::<8>(TYPED_ARRAY_WITH_RAB_SOURCE, true);
}

#[test]
fn typed_array_with_accepts_all_standard_source_factories() {
    assert_typed_array_with::<1>(TYPED_ARRAY_WITH_SOURCE_FACTORIES, false);
    assert_typed_array_with::<2>(TYPED_ARRAY_WITH_SOURCE_FACTORIES, false);
    assert_typed_array_with::<4>(TYPED_ARRAY_WITH_SOURCE_FACTORIES, false);
    assert_typed_array_with::<8>(TYPED_ARRAY_WITH_SOURCE_FACTORIES, false);
    assert_typed_array_with::<16>(TYPED_ARRAY_WITH_SOURCE_FACTORIES, false);
    assert_typed_array_with::<8>(TYPED_ARRAY_WITH_SOURCE_FACTORIES, true);
}

/// Executes one with fixture under the selected dispatch and collection policy.
fn assert_typed_array_with<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_with_fixture(source);
    let mut isolate = typed_array_with_test_isolate(source);
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
                fuel: 1_000_000,
                quantum: 1_000_000,
            },
        )
        .expect("TypedArray with fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Gives only the all-constructor high-water fixture one extra span for its simultaneous backings.
fn typed_array_with_test_isolate(source: &'static str) -> Isolate {
    if source != TYPED_ARRAY_WITH_SOURCE_FACTORIES {
        return test_isolate();
    }
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(10 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("large TypedArray fixture isolate initializes")
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

/// Compiles one fixture independently of VM scheduling policy.
fn compile_typed_array_with_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_439),
                SourceName::new("typed-array-with-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray with fixture compiles")
}
