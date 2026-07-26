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
var names = ["every", "some", "find", "findIndex", "findLast", "findLastIndex"];
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
valuesOkay && metadataOkay && rejected === 6 && nonCallable &&
  detachedCalls === 2 && detachedSecond === undefined;
"#;

const TYPED_ARRAY_CALLBACK_GC_SOURCE: &str = r#"
var array = new Uint8Array([1, 2, 3, 4]);
var seen = 0;
var result = array.find(function(value, index, receiver) {
  seen += value + index;
  receiver[index] = value + 1;
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
result === 3 && seen === 9 && detachedCalls === 2 && detachedSecond === undefined;
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
