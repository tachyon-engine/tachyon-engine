use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_FILL_SOURCE: &str = r#"
function verify(TA) {
  var array = new TA([1, 2, 3, 4]);
  var returned = array.fill(7, 1, -1);
  return returned === array && array[0] === 1 && array[1] === 7 && array[2] === 7 && array[3] === 4;
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var valuesOkay = true;
for (var i = 0; i < constructors.length; i++) valuesOkay = valuesOkay && verify(constructors[i]);

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "fill");
var metadataOkay = property.value.name === "fill" && property.value.length === 1 &&
  property.writable === true && property.enumerable === false && property.configurable === true;
var rejected = false;
try { property.value.call({}, 1); } catch (error) { rejected = error instanceof TypeError; }

var order = "";
var conversions = 0;
var ordered = new Uint8Array([0, 0, 0, 0]);
ordered.fill(
  { valueOf: function() { order += "v"; conversions++; return 9; } },
  { valueOf: function() { order += "s"; return 1; } },
  { valueOf: function() { order += "e"; return 3; } }
);
var abrupt = {};
var abruptIdentity = false;
try {
  ordered.fill(
    { valueOf: function() { order += "V"; return 1; } },
    { valueOf: function() { throw abrupt; } },
    { valueOf: function() { order += "E"; return 4; } }
  );
} catch (error) { abruptIdentity = error === abrupt; }

valuesOkay && metadataOkay && rejected && order === "vseV" && conversions === 1 &&
  ordered[0] === 0 && ordered[1] === 9 && ordered[2] === 9 && ordered[3] === 0 && abruptIdentity;
"#;

const TYPED_ARRAY_FILL_GC_SOURCE: &str = r#"
var order = "";
var array = new Uint8Array([1, 2, 3, 4]);
var threw = false;
try {
  array.fill(
    { valueOf: function() { order += "v"; $262.detachArrayBuffer(array.buffer); return 8; } },
    { valueOf: function() { order += "s"; return 1; } },
    { valueOf: function() { order += "e"; return 3; } }
  );
} catch (error) { threw = error instanceof TypeError; }

var startArray = new Uint8Array([1, 2]);
var startThrew = false;
try {
  startArray.fill(5, { valueOf: function() {
    $262.detachArrayBuffer(startArray.buffer);
    return 0;
  }});
} catch (error) { startThrew = error instanceof TypeError; }

var endArray = new Uint8Array([1, 2]);
var endThrew = false;
try {
  endArray.fill(5, 0, { valueOf: function() {
    $262.detachArrayBuffer(endArray.buffer);
    return 2;
  }});
} catch (error) { endThrew = error instanceof TypeError; }

order === "vse" && threw && startThrew && endThrew;
"#;

const TYPED_ARRAY_FILL_LONG_SOURCE: &str = r#"
var length = 20000;
var array = new Uint8Array(length);
var returned = array.fill(37);
returned === array && array[0] === 37 && array[9999] === 37 && array[length - 1] === 37;
"#;

#[test]
fn typed_array_fill_works_for_every_dispatch_batch() {
    assert_typed_array_fill::<1>(TYPED_ARRAY_FILL_SOURCE, false);
    assert_typed_array_fill::<2>(TYPED_ARRAY_FILL_SOURCE, false);
    assert_typed_array_fill::<4>(TYPED_ARRAY_FILL_SOURCE, false);
    assert_typed_array_fill::<8>(TYPED_ARRAY_FILL_SOURCE, false);
    assert_typed_array_fill::<16>(TYPED_ARRAY_FILL_SOURCE, false);
}

#[test]
fn typed_array_fill_conversion_state_survives_forced_major_collection() {
    assert_typed_array_fill::<8>(TYPED_ARRAY_FILL_GC_SOURCE, true);
}

#[test]
fn typed_array_fill_bulk_loop_does_not_grow_rust_stack() {
    assert_typed_array_fill::<8>(TYPED_ARRAY_FILL_LONG_SOURCE, false);
}

/// Executes one fill fixture under the selected dispatch and collection policy.
fn assert_typed_array_fill<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_fill_fixture(source);
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
        .expect("TypedArray fill fixture executes");
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

/// Compiles one fill fixture independently of VM scheduling policy.
fn compile_typed_array_fill_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_431),
                SourceName::new("typed-array-fill-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray fill fixture compiles")
}
