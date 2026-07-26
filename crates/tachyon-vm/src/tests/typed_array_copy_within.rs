use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_COPY_WITHIN_SOURCE: &str = r#"
function verify(TA) {
  var backwards = new TA([0, 1, 2, 3, 4]);
  var backwardsReturn = backwards.copyWithin(1, 0, 4);
  var forwards = new TA([0, 1, 2, 3, 4]);
  var forwardsReturn = forwards.copyWithin(0, 1, 5);
  return backwardsReturn === backwards && forwardsReturn === forwards &&
    backwards[0] === 0 && backwards[1] === 0 && backwards[2] === 1 &&
    backwards[3] === 2 && backwards[4] === 3 &&
    forwards[0] === 1 && forwards[1] === 2 && forwards[2] === 3 &&
    forwards[3] === 4 && forwards[4] === 4;
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var valuesOkay = true;
for (var i = 0; i < constructors.length; i++) valuesOkay = valuesOkay && verify(constructors[i]);

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "copyWithin");
var metadataOkay = property.value.name === "copyWithin" && property.value.length === 2 &&
  property.writable === true && property.enumerable === false && property.configurable === true;
var rejected = false;
try { property.value.call({}, 0, 1); } catch (error) { rejected = error instanceof TypeError; }

var order = "";
var ordered = new Uint8Array([0, 1, 2, 3]);
ordered.copyWithin(
  { valueOf: function() { order += "t"; return 0; } },
  { valueOf: function() { order += "s"; return 1; } },
  { valueOf: function() { order += "e"; return 3; } }
);
var abrupt = {};
var abruptIdentity = false;
try {
  ordered.copyWithin(
    { valueOf: function() { order += "T"; return 0; } },
    { valueOf: function() { throw abrupt; } },
    { valueOf: function() { order += "E"; return 3; } }
  );
} catch (error) { abruptIdentity = error === abrupt; }

var buffer = new ArrayBuffer(8);
var whole = new Uint8Array(buffer);
for (var j = 0; j < whole.length; j++) whole[j] = j + 1;
var offsetView = new Uint8Array(buffer, 2, 4);
offsetView.copyWithin(1, 0, 3);
var byteOffsetOkay = whole[0] === 1 && whole[1] === 2 && whole[2] === 3 &&
  whole[3] === 3 && whole[4] === 4 && whole[5] === 5 && whole[6] === 7 && whole[7] === 8;

valuesOkay && metadataOkay && rejected && order === "tseT" && abruptIdentity &&
  ordered[0] === 1 && ordered[1] === 2 && ordered[2] === 2 && ordered[3] === 3 && byteOffsetOkay;
"#;

const TYPED_ARRAY_COPY_WITHIN_GC_SOURCE: &str = r#"
var order = "";
var array = new Uint8Array([1, 2, 3, 4]);
var targetThrew = false;
try {
  array.copyWithin(
    { valueOf: function() { order += "t"; $262.detachArrayBuffer(array.buffer); return 0; } },
    { valueOf: function() { order += "s"; return 1; } },
    { valueOf: function() { order += "e"; return 3; } }
  );
} catch (error) { targetThrew = error instanceof TypeError; }

var startArray = new Uint8Array([1, 2, 3]);
var startThrew = false;
try {
  startArray.copyWithin(0, { valueOf: function() {
    $262.detachArrayBuffer(startArray.buffer);
    return 1;
  }}, 3);
} catch (error) { startThrew = error instanceof TypeError; }

var endArray = new Uint8Array([1, 2, 3]);
var endThrew = false;
try {
  endArray.copyWithin(0, 1, { valueOf: function() {
    $262.detachArrayBuffer(endArray.buffer);
    return 3;
  }});
} catch (error) { endThrew = error instanceof TypeError; }

var zeroCount = new Uint8Array([1, 2]);
var zeroCountReturned;
var zeroCountThrew = false;
try {
  zeroCountReturned = zeroCount.copyWithin({ valueOf: function() {
    $262.detachArrayBuffer(zeroCount.buffer);
    return 2;
  }}, 0, 0);
} catch (error) { zeroCountThrew = true; }

order === "tse" && targetThrew && startThrew && endThrew &&
  !zeroCountThrew && zeroCountReturned === zeroCount;
"#;

const TYPED_ARRAY_COPY_WITHIN_LONG_SOURCE: &str = r#"
var length = 20000;
var array = new Uint8Array(length);
array[0] = 11;
array[1] = 22;
array[length - 2] = 33;
array[length - 1] = 44;
var returned = array.copyWithin(1, 0, length - 1);
returned === array && array[0] === 11 && array[1] === 11 && array[2] === 22 &&
  array[length - 1] === 33;
"#;

#[test]
fn typed_array_copy_within_works_for_every_dispatch_batch() {
    assert_typed_array_copy_within::<1>(TYPED_ARRAY_COPY_WITHIN_SOURCE, false);
    assert_typed_array_copy_within::<2>(TYPED_ARRAY_COPY_WITHIN_SOURCE, false);
    assert_typed_array_copy_within::<4>(TYPED_ARRAY_COPY_WITHIN_SOURCE, false);
    assert_typed_array_copy_within::<8>(TYPED_ARRAY_COPY_WITHIN_SOURCE, false);
    assert_typed_array_copy_within::<16>(TYPED_ARRAY_COPY_WITHIN_SOURCE, false);
}

#[test]
fn typed_array_copy_within_state_survives_forced_major_collection() {
    assert_typed_array_copy_within::<8>(TYPED_ARRAY_COPY_WITHIN_GC_SOURCE, true);
}

#[test]
fn typed_array_copy_within_bulk_loop_does_not_grow_rust_stack() {
    assert_typed_array_copy_within::<8>(TYPED_ARRAY_COPY_WITHIN_LONG_SOURCE, false);
}

/// Executes one copyWithin fixture under the selected dispatch and collection policy.
fn assert_typed_array_copy_within<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_copy_within_fixture(source);
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
                fuel: 1_000_000,
                quantum: 1_000_000,
            },
        )
        .expect("TypedArray copyWithin fixture executes");
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

/// Compiles one copyWithin fixture independently of VM scheduling policy.
fn compile_typed_array_copy_within_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_432),
                SourceName::new("typed-array-copy-within-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray copyWithin fixture compiles")
}
