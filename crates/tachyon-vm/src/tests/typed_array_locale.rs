use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const BASIC_SOURCE: &str = r#"
var numberCalls = 0;
var bigintCalls = 0;
Number.prototype.toLocaleString = function() {
  numberCalls++;
  return "n" + this.valueOf();
};
BigInt.prototype.toLocaleString = function() {
  bigintCalls++;
  return "b" + this.valueOf().toString();
};
var numberConstructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var numberOkay = true;
for (var i = 0; i < numberConstructors.length; i++) {
  numberOkay = numberOkay && new numberConstructors[i]([1, 2]).toLocaleString("ignored", {}) === "n1,n2";
}
var bigintOkay = new BigInt64Array([1n, 2n]).toLocaleString() === "b1,b2" &&
  new BigUint64Array([1n, 2n]).toLocaleString() === "b1,b2";
var typedArrayPrototype = Object.getPrototypeOf(Int8Array).prototype;
var descriptor = Object.getOwnPropertyDescriptor(typedArrayPrototype, "toLocaleString");
var metadataOkay = descriptor.value.name === "toLocaleString" && descriptor.value.length === 0 &&
  descriptor.writable === true && descriptor.enumerable === false && descriptor.configurable === true;
var rejected = false;
try { descriptor.value.call([]); } catch (error) { rejected = error instanceof TypeError; }
numberOkay && bigintOkay && numberCalls === 18 && bigintCalls === 4 && metadataOkay && rejected;
"#;

const RESIZE_SOURCE: &str = r#"
function growCase(tracking) {
  var rab = new ArrayBuffer(4, { maxByteLength: 8 });
  var target = tracking ? new Uint8Array(rab) : new Uint8Array(rab, 0, 4);
  var calls = 0;
  Number.prototype.toLocaleString = function() {
    calls++;
    if (calls === 2) rab.resize(6);
    return "0";
  };
  return target.toLocaleString() === "0,0,0,0" && calls === 4;
}
function shrinkCase(tracking) {
  var rab = new ArrayBuffer(4, { maxByteLength: 8 });
  var target = tracking ? new Uint8Array(rab) : new Uint8Array(rab, 0, 4);
  var calls = 0;
  Number.prototype.toLocaleString = function() {
    calls++;
    if (calls === 2) rab.resize(2);
    return "0";
  };
  return target.toLocaleString() === "0,0,," && calls === 2;
}
var abrupt = {};
var abruptOkay = false;
Number.prototype.toLocaleString = function() { throw abrupt; };
try { new Uint8Array([1]).toLocaleString(); } catch (error) { abruptOkay = error === abrupt; }
var detached = new Uint8Array(1);
$262.detachArrayBuffer(detached.buffer);
var detachedOkay = false;
try { detached.toLocaleString(); } catch (error) { detachedOkay = error instanceof TypeError; }
growCase(false) && growCase(true) && shrinkCase(false) && shrinkCase(true) && abruptOkay && detachedOkay;
"#;

#[test]
fn typed_array_to_locale_string_basic_number_path() {
    assert_typed_array_locale::<1>(BASIC_SOURCE, false);
    assert_typed_array_locale::<2>(BASIC_SOURCE, false);
    assert_typed_array_locale::<4>(BASIC_SOURCE, false);
    assert_typed_array_locale::<8>(BASIC_SOURCE, false);
    assert_typed_array_locale::<16>(BASIC_SOURCE, false);
}

#[test]
fn typed_array_to_locale_string_resize_abrupt_and_detach_survive_forced_major() {
    assert_typed_array_locale::<8>(RESIZE_SOURCE, true);
}

/// Executes one TypedArray locale fixture under a selected dispatch and GC policy.
fn assert_typed_array_locale<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_locale_fixture(source);
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
        .expect("TypedArray locale fixture executes");
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

/// Compiles one locale fixture independently of VM scheduling policy.
fn compile_typed_array_locale_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_436),
                SourceName::new("typed-array-locale-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray locale fixture compiles")
}
