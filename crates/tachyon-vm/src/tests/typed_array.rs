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

/// Compiles the shared fixture independently of dispatch and collection policy.
fn compile_typed_array_fixture() -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_421),
                SourceName::new("typed-array-fixture"),
                MediaType::JavaScript,
                Arc::from(TYPED_ARRAY_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray fixture compiles")
}
