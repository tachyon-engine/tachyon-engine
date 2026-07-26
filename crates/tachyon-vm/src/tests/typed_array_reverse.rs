use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const TYPED_ARRAY_REVERSE_SOURCE: &str = r#"
function verify(TA) {
  var odd = new TA([0, 1, 2, 3, 4]);
  odd.note = 91;
  var oddReturn = odd.reverse();
  var even = new TA([0, 1, 2, 3]);
  var evenReturn = even.reverse();
  return oddReturn === odd && evenReturn === even && odd.note === 91 &&
    odd[0] === 4 && odd[1] === 3 && odd[2] === 2 && odd[3] === 1 && odd[4] === 0 &&
    even[0] === 3 && even[1] === 2 && even[2] === 1 && even[3] === 0;
}
var constructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var valuesOkay = true;
for (var i = 0; i < constructors.length; i++) valuesOkay = valuesOkay && verify(constructors[i]);

var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var property = Object.getOwnPropertyDescriptor(typedArrayPrototype, "reverse");
var metadataOkay = property.value.name === "reverse" && property.value.length === 0 &&
  property.writable === true && property.enumerable === false && property.configurable === true;
var rejected = false;
try { property.value.call({}, 1); } catch (error) { rejected = error instanceof TypeError; }

var offsetBuffer = new ArrayBuffer(10);
var offsetBytes = new Uint8Array(offsetBuffer);
for (var j = 0; j < offsetBytes.length; j++) offsetBytes[j] = j + 1;
new Uint16Array(offsetBuffer, 2, 4).reverse();
var byteOffsetOkay = offsetBytes[0] === 1 && offsetBytes[1] === 2 &&
  offsetBytes[2] === 9 && offsetBytes[3] === 10 && offsetBytes[4] === 7 &&
  offsetBytes[5] === 8 && offsetBytes[6] === 5 && offsetBytes[7] === 6 &&
  offsetBytes[8] === 3 && offsetBytes[9] === 4;

var bitsBuffer = new ArrayBuffer(16);
var words = new Uint32Array(bitsBuffer);
words[0] = 0x12345678;
words[1] = 0x7ff80000;
words[2] = 0x87654321;
words[3] = 0x7ff80000;
new Float64Array(bitsBuffer).reverse();
var bitsOkay = words[0] === 0x87654321 && words[1] === 0x7ff80000 &&
  words[2] === 0x12345678 && words[3] === 0x7ff80000;

valuesOkay && metadataOkay && rejected && byteOffsetOkay && bitsOkay;
"#;

const TYPED_ARRAY_REVERSE_DETACH_SOURCE: &str = r#"
var array = new Uint8Array([1, 2, 3]);
$262.detachArrayBuffer(array.buffer);
var threw = false;
try { array.reverse(); } catch (error) { threw = error instanceof TypeError; }
threw;
"#;

const TYPED_ARRAY_REVERSE_LONG_SOURCE: &str = r#"
var length = 20000;
var array = new Uint32Array(length);
array[0] = 11;
array[1] = 22;
array[length - 2] = 33;
array[length - 1] = 44;
var returned = array.reverse();
returned === array && array[0] === 44 && array[1] === 33 &&
  array[length - 2] === 22 && array[length - 1] === 11;
"#;

#[test]
fn typed_array_reverse_works_for_every_dispatch_batch() {
    assert_typed_array_reverse::<1>(TYPED_ARRAY_REVERSE_SOURCE, false);
    assert_typed_array_reverse::<2>(TYPED_ARRAY_REVERSE_SOURCE, false);
    assert_typed_array_reverse::<4>(TYPED_ARRAY_REVERSE_SOURCE, false);
    assert_typed_array_reverse::<8>(TYPED_ARRAY_REVERSE_SOURCE, false);
    assert_typed_array_reverse::<16>(TYPED_ARRAY_REVERSE_SOURCE, false);
}

#[test]
fn typed_array_reverse_rejects_detached_backing_under_forced_major_collection() {
    assert_typed_array_reverse::<8>(TYPED_ARRAY_REVERSE_DETACH_SOURCE, true);
}

#[test]
fn typed_array_reverse_large_view_does_not_grow_rust_stack() {
    assert_typed_array_reverse::<8>(TYPED_ARRAY_REVERSE_LONG_SOURCE, false);
}

/// Executes one reverse fixture under the selected dispatch and collection policy.
fn assert_typed_array_reverse<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_reverse_fixture(source);
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
        .expect("TypedArray reverse fixture executes");
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

/// Compiles one reverse fixture independently of VM scheduling policy.
fn compile_typed_array_reverse_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_433),
                SourceName::new("typed-array-reverse-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray reverse fixture compiles")
}
