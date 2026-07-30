use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_JOIN_SOURCE: &str = r#"
function verify(TA) {
  var target = new TA([1, 2, 3]);
  return target.join("|") === "1|2|3" && target.join() === "1,2,3" &&
    target.join("") === "123";
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var constructorsOkay = true;
for (var i = 0; i < constructors.length; i++) constructorsOkay = constructorsOkay && verify(constructors[i]);

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "join");
var metadataOkay = property.value.name === "join" && property.value.length === 1 &&
  property.writable === true && property.enumerable === false && property.configurable === true;
var rejected = false;
try { property.value.call({}); } catch (error) { rejected = error instanceof TypeError; }

var offsetBuffer = new ArrayBuffer(8);
var offsetTarget = new Uint8Array(offsetBuffer, 2, 4);
offsetTarget[0] = 7; offsetTarget[1] = 8; offsetTarget[2] = 9; offsetTarget[3] = 10;
var offsetOkay = offsetTarget.join("/") === "7/8/9/10";

var special = new Float64Array([NaN, Infinity, -Infinity, -0]);
var specialOkay = special.join(",") === "NaN,Infinity,-Infinity,0";

var order = "";
var separator = { toString: function() { order += "s"; return ":"; } };
var observableOkay = new Uint8Array([4, 5]).join(separator) === "4:5" && order === "s";
var abrupt = {};
var abruptIdentity = false;
try { new Uint8Array(0).join({ toString: function() { throw abrupt; } }); }
catch (error) { abruptIdentity = error === abrupt; }

constructorsOkay && metadataOkay && rejected && offsetOkay && specialOkay &&
  observableOkay && abruptIdentity;
"#;

const TYPED_ARRAY_JOIN_DETACH_SOURCE: &str = r#"
var array = new Uint8Array([1, 2, 3]);
var separator = { toString: function() {
  $262.detachArrayBuffer(array.buffer);
  return ",";
} };
var detachedResult = array.join(separator) === ",,";
var detached = new Uint8Array(1);
$262.detachArrayBuffer(detached.buffer);
var detachedThrows = false;
try { detached.join(); } catch (error) { detachedThrows = error instanceof TypeError; }
detachedResult && detachedThrows;
"#;

const TYPED_ARRAY_JOIN_LONG_SOURCE: &str = r#"
var length = 20000;
var array = new Uint32Array(length);
array[0] = 11;
array[1] = 22;
array[length - 2] = 33;
array[length - 1] = 44;
var result = array.join(";");
result.length === 40003 &&
  result.charCodeAt(0) === 49 && result.charCodeAt(1) === 49 && result.charCodeAt(2) === 59 &&
  result.charCodeAt(3) === 50 && result.charCodeAt(4) === 50 && result.charCodeAt(5) === 59 &&
  result.charCodeAt(result.length - 6) === 59 && result.charCodeAt(result.length - 5) === 51 &&
  result.charCodeAt(result.length - 4) === 51 && result.charCodeAt(result.length - 3) === 59 &&
  result.charCodeAt(result.length - 2) === 52 && result.charCodeAt(result.length - 1) === 52;
"#;

#[test]
fn typed_array_join_works_for_every_dispatch_batch() {
    assert_typed_array_join::<1>(TYPED_ARRAY_JOIN_SOURCE, false);
    assert_typed_array_join::<2>(TYPED_ARRAY_JOIN_SOURCE, false);
    assert_typed_array_join::<4>(TYPED_ARRAY_JOIN_SOURCE, false);
    assert_typed_array_join::<8>(TYPED_ARRAY_JOIN_SOURCE, false);
    assert_typed_array_join::<16>(TYPED_ARRAY_JOIN_SOURCE, false);
}

#[test]
fn typed_array_join_detach_and_separator_survive_forced_major_collection() {
    assert_typed_array_join::<8>(TYPED_ARRAY_JOIN_DETACH_SOURCE, true);
}

#[test]
fn typed_array_join_large_view_uses_exact_output_capacity() {
    assert_typed_array_join::<8>(TYPED_ARRAY_JOIN_LONG_SOURCE, false);
}

/// Executes one join fixture under the selected dispatch and collection policy.
fn assert_typed_array_join<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_join_fixture(source);
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
        .expect("TypedArray join fixture executes");
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
    _kind: crate::DynamicFunctionKind,
    _source: crate::DynamicFunctionSource,
) -> Result<Value, ExecutionError> {
    Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
}

/// Compiles one join fixture independently of VM scheduling policy.
fn compile_typed_array_join_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_435),
                SourceName::new("typed-array-join-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray join fixture compiles")
}
