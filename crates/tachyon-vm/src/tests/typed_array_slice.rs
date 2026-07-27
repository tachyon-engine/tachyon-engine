use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_SLICE_SOURCE: &str = r#"
function verify(TA) {
  var source = new TA([1, 2, 3, 4, 5]);
  source.note = 91;
  var result = source.slice(1, -1);
  return result !== source && result.length === 3 && result[0] === 2 &&
    result[1] === 3 && result[2] === 4 && result.note === undefined;
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var constructorsOkay = true;
for (var i = 0; i < constructors.length; i++) constructorsOkay = constructorsOkay && verify(constructors[i]);

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "slice");
var metadataOkay = property.value.name === "slice" && property.value.length === 2 &&
  property.writable === true && property.enumerable === false && property.configurable === true;
var rejected = false;
try { property.value.call({}); } catch (error) { rejected = error instanceof TypeError; }

var order = "";
var ordered = new Uint8Array([10, 20, 30, 40]);
var orderedResult = ordered.slice(
  { valueOf: function() { order += "s"; return 1; } },
  { valueOf: function() { order += "e"; return 3; } }
);
var orderOkay = order === "se" && orderedResult.length === 2 &&
  orderedResult[0] === 20 && orderedResult[1] === 30;

var cross = new Uint16Array([7, 8, 9]);
cross.constructor = {};
cross.constructor[Symbol.species] = function(count) { return new Float64Array(count); };
var crossResult = cross.slice(1);
var crossOkay = crossResult instanceof Float64Array && crossResult.length === 2 &&
  crossResult[0] === 8 && crossResult[1] === 9;

var overlapBuffer = new ArrayBuffer(8);
var overlap = new Uint8Array(overlapBuffer, 0, 6);
for (var j = 0; j < overlap.length; j++) overlap[j] = j + 1;
overlap.constructor = {};
overlap.constructor[Symbol.species] = function(count) {
  return new Uint8Array(overlapBuffer, 2, count);
};
var overlapResult = overlap.slice(0, 4);
var bytes = new Uint8Array(overlapBuffer);
var overlapOkay = overlapResult[0] === 1 && overlapResult[1] === 2 &&
  overlapResult[2] === 1 && overlapResult[3] === 2 &&
  bytes[0] === 1 && bytes[1] === 2 && bytes[2] === 1 && bytes[3] === 2 &&
  bytes[4] === 1 && bytes[5] === 2;

var shortTargetThrows = false;
var shortSource = new Uint8Array([1, 2]);
shortSource.constructor = {};
shortSource.constructor[Symbol.species] = function() { return new Uint8Array(); };
try { shortSource.slice(); }
catch (error) { shortTargetThrows = error instanceof TypeError; }

var bitsSourceBuffer = new ArrayBuffer(16);
var bitsSourceWords = new Uint32Array(bitsSourceBuffer);
bitsSourceWords[0] = 0x12345678; bitsSourceWords[1] = 0x7ff80000;
bitsSourceWords[2] = 0x87654321; bitsSourceWords[3] = 0x7ff80000;
var bitsResult = new Float64Array(bitsSourceBuffer).slice();
var bitsWords = new Uint32Array(bitsResult.buffer);
var bitsOkay = bitsWords[0] === 0x12345678 && bitsWords[1] === 0x7ff80000 &&
  bitsWords[2] === 0x87654321 && bitsWords[3] === 0x7ff80000;

var bigintSource = new BigInt64Array([-1n, 2n, 3n]);
bigintSource.constructor = {};
bigintSource.constructor[Symbol.species] = function(count) { return new BigUint64Array(count); };
var bigintResult = bigintSource.slice(0, 2);
var bigintOkay = bigintResult instanceof BigUint64Array &&
  bigintResult[0] === 18446744073709551615n && bigintResult[1] === 2n;

function rejectsContentType(source, Target) {
  source.constructor = {};
  source.constructor[Symbol.species] = function(count) { return new Target(count); };
  try { source.slice(); } catch (error) { return error instanceof TypeError; }
  return false;
}
var contentTypeMismatch = rejectsContentType(new BigInt64Array(0), Uint8Array) &&
  rejectsContentType(new Uint8Array(0), BigInt64Array);

constructorsOkay && metadataOkay && rejected && orderOkay && crossOkay &&
  overlapOkay && shortTargetThrows && bitsOkay && bigintOkay && contentTypeMismatch;
"#;

const TYPED_ARRAY_SLICE_DETACH_SOURCE: &str = r#"
var nonempty = new Uint8Array([1]);
nonempty.constructor = {};
nonempty.constructor[Symbol.species] = function(count) {
  $262.detachArrayBuffer(nonempty.buffer);
  return new Uint8Array(count);
};
var nonemptyThrows = false;
try { nonempty.slice(); } catch (error) { nonemptyThrows = error instanceof TypeError; }

var empty = new Uint8Array(0);
empty.constructor = {};
empty.constructor[Symbol.species] = function(count) {
  $262.detachArrayBuffer(empty.buffer);
  return new Uint8Array(count);
};
var emptyResult = empty.slice();

var abrupt = {};
var abruptIdentity = false;
try { new Uint8Array(1).slice({ valueOf: function() { throw abrupt; } }); }
catch (error) { abruptIdentity = error === abrupt; }

nonemptyThrows && emptyResult.length === 0 && abruptIdentity;
"#;

const TYPED_ARRAY_SLICE_LONG_SOURCE: &str = r#"
var length = 20000;
var source = new Uint32Array(length);
source[0] = 11;
source[1] = 22;
source[length - 2] = 33;
source[length - 1] = 44;
var result = source.slice();
result.length === length && result.buffer !== source.buffer &&
  result[0] === 11 && result[1] === 22 &&
  result[length - 2] === 33 && result[length - 1] === 44;
"#;

#[test]
fn typed_array_slice_works_for_every_dispatch_batch() {
    assert_typed_array_slice::<1>(TYPED_ARRAY_SLICE_SOURCE, false);
    assert_typed_array_slice::<2>(TYPED_ARRAY_SLICE_SOURCE, false);
    assert_typed_array_slice::<4>(TYPED_ARRAY_SLICE_SOURCE, false);
    assert_typed_array_slice::<8>(TYPED_ARRAY_SLICE_SOURCE, false);
    assert_typed_array_slice::<16>(TYPED_ARRAY_SLICE_SOURCE, false);
}

#[test]
fn typed_array_slice_species_state_survives_forced_major_collection() {
    assert_typed_array_slice::<8>(TYPED_ARRAY_SLICE_DETACH_SOURCE, true);
}

#[test]
fn typed_array_slice_large_view_does_not_grow_rust_stack() {
    let module = compile_typed_array_slice_fixture(TYPED_ARRAY_SLICE_LONG_SOURCE);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(4_096, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(32 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 4_096).with_max_shapes(4_096),
    ))
    .expect("large TypedArray slice isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_000_000,
                quantum: 4_000_000,
            },
        )
        .expect("large TypedArray slice executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "large TypedArray slice returned {outcome:?}"
    );
}

/// Executes one slice fixture under the selected dispatch and collection policy.
fn assert_typed_array_slice<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_slice_fixture(source);
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
        .expect("TypedArray slice fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
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

/// Compiles one slice fixture independently of VM scheduling policy.
fn compile_typed_array_slice_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_436),
                SourceName::new("typed-array-slice-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray slice fixture compiles")
}
