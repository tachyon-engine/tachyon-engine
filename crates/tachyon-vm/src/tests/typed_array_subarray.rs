use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_SUBARRAY_SOURCE: &str = r#"
function verify(TA) {
  var source = new TA([10, 20, 30, 40]);
  source.note = 91;
  var result = source.subarray(1, -1);
  var initial = result !== source && result.buffer === source.buffer &&
    result.length === 2 && result[0] === 20 && result[1] === 30 &&
    result.note === undefined;
  source[1] = 70;
  var sourceMutation = result[0] === 70;
  result[1] = 80;
  return initial && sourceMutation && source[2] === 80;
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var constructorsOkay = true;
for (var i = 0; i < constructors.length; i++) {
  constructorsOkay = constructorsOkay && verify(constructors[i]);
}

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "subarray");
var metadataOkay = property.value.name === "subarray" && property.value.length === 2 &&
  property.writable === true && property.enumerable === false && property.configurable === true;
var rejected = false;
try { property.value.call({}); } catch (error) { rejected = error instanceof TypeError; }

var order = "";
var ordered = new Uint16Array([11, 22, 33, 44]);
var holder = {};
Object.defineProperty(holder, Symbol.species, {
  get: function() {
    order += "p";
    return function(buffer, offset, length) {
      order += "c";
      var argumentsOkay = buffer === ordered.buffer &&
        offset === Uint16Array.BYTES_PER_ELEMENT && length === 2;
      return argumentsOkay ? new Uint16Array(buffer, offset, length) : new Uint16Array();
    };
  }
});
ordered.constructor = holder;
var orderedResult = ordered.subarray(
  { valueOf: function() { order += "s"; return 1; } },
  { valueOf: function() { order += "e"; return 3; } }
);
var orderOkay = order === "sepc" && orderedResult.length === 2 &&
  orderedResult[0] === 22 && orderedResult[1] === 33;

var unrelated = new Float64Array([9]);
var custom = new Uint8Array([1, 2, 3]);
custom.constructor = {};
custom.constructor[Symbol.species] = function() { return unrelated; };
var customOkay = custom.subarray(1) === unrelated;

var initiallyDetached = new Uint8Array(2);
$262.detachArrayBuffer(initiallyDetached.buffer);
var detachedOrder = "";
var detachedThrows = false;
try {
  initiallyDetached.subarray(
    { valueOf: function() { detachedOrder += "s"; return 0; } },
    { valueOf: function() { detachedOrder += "e"; return 0; } }
  );
} catch (error) { detachedThrows = error instanceof TypeError; }

var detachedDuring = new Uint8Array([5, 6]);
var detachedReplacement = new Float32Array([17]);
var detachedBuffer = detachedDuring.buffer;
var observedDetachedArguments = false;
detachedDuring.constructor = {};
detachedDuring.constructor[Symbol.species] = function(buffer, offset, length) {
  observedDetachedArguments = buffer === detachedBuffer && offset === 1 && length === 0;
  return detachedReplacement;
};
var detachedDuringResult = detachedDuring.subarray(1, {
  valueOf: function() { $262.detachArrayBuffer(detachedBuffer); return 0; }
});
var detachedCustomOkay = detachedDuringResult === detachedReplacement &&
  observedDetachedArguments;

constructorsOkay && metadataOkay && rejected && orderOkay && customOkay &&
  detachedThrows && detachedOrder === "se" && detachedCustomOkay;
"#;

const TYPED_ARRAY_SUBARRAY_CROSS_REALM_SOURCE: &str = r#"
var source = new Uint16Array([10, 20, 30, 40]);
source.constructor = foreignUint16Array;
var result = source.subarray(1, 3);
globalThis.crossRealmSubarrayResult = result;
result.buffer === source.buffer && result.length === 2 &&
  result[0] === 20 && result[1] === 30;
"#;

#[test]
fn typed_array_subarray_works_for_every_dispatch_batch() {
    assert_typed_array_subarray::<1>(false);
    assert_typed_array_subarray::<2>(false);
    assert_typed_array_subarray::<4>(false);
    assert_typed_array_subarray::<8>(false);
    assert_typed_array_subarray::<16>(false);
}

#[test]
fn typed_array_subarray_state_survives_forced_major_collection() {
    assert_typed_array_subarray::<8>(true);
}

#[test]
fn typed_array_subarray_constructs_foreign_species_in_its_realm() {
    let module =
        compile_typed_array_subarray_fixture(TYPED_ARRAY_SUBARRAY_CROSS_REALM_SOURCE, 7_438);
    let mut isolate = test_isolate();
    let (_, child_global) = isolate.create_realm().expect("child Realm initializes");
    let constructor_atom = isolate.intern_intrinsic_name(b"Uint16Array").unwrap();
    let foreign_constructor = isolate
        .get_data_property(child_global, constructor_atom)
        .unwrap()
        .expect("child Realm publishes Uint16Array");
    let prototype_atom = isolate.intern_intrinsic_name(b"prototype").unwrap();
    let foreign_prototype = isolate
        .get_data_property(foreign_constructor, prototype_atom)
        .unwrap()
        .expect("foreign Uint16Array publishes prototype");
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
        .expect("cross-Realm TypedArray subarray executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "cross-Realm TypedArray subarray returned {outcome:?}"
    );
    let result_atom = isolate
        .intern_intrinsic_name(b"crossRealmSubarrayResult")
        .unwrap();
    let result = isolate
        .get_data_property(global, result_atom)
        .unwrap()
        .expect("fixture publishes cross-Realm result");
    assert_eq!(
        isolate.object_prototype_of(result).unwrap(),
        foreign_prototype,
        "foreign TypedArray species must use its constructor Realm prototype"
    );
}

/// Executes the shared fixture under one dispatch and collection policy.
fn assert_typed_array_subarray<const N: usize>(forced_major: bool) {
    let module = compile_typed_array_subarray_fixture(TYPED_ARRAY_SUBARRAY_SOURCE, 7_437);
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
        .expect("TypedArray subarray fixture executes");
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

/// Compiles one subarray fixture independently of VM scheduling policy.
fn compile_typed_array_subarray_fixture(source: &'static str, id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(id),
                SourceName::new("typed-array-subarray-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray subarray fixture compiles")
}
