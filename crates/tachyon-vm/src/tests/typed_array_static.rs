use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STATIC_SOURCE: &str = r#"
var numberConstructors = [
  Float64Array, Float32Array, Int32Array, Int16Array, Int8Array,
  Uint32Array, Uint16Array, Uint8Array, Uint8ClampedArray
];
var numberOkay = true;
for (var i = 0; i < numberConstructors.length; i++) {
  var TA = numberConstructors[i];
  var thisArg = { offset: 3 };
  var mapped = TA.from([1, 2], function(value, index) {
    return value + index + this.offset;
  }, thisArg);
  var created = TA.of(5, 6);
  numberOkay = numberOkay && mapped.length === 2 && mapped[0] === 4 && mapped[1] === 6 &&
    created.length === 2 && created[0] === 5 && created[1] === 6;
}
var bigintMapped = BigInt64Array.from([1n, 2n], function(value, index) {
  return value + BigInt(index);
});
var bigintCreated = BigUint64Array.of(3n, 4n);
var TypedArray = Object.getPrototypeOf(Int8Array);
var fromDescriptor = Object.getOwnPropertyDescriptor(TypedArray, "from");
var ofDescriptor = Object.getOwnPropertyDescriptor(TypedArray, "of");
var metadataOkay = fromDescriptor.value.length === 1 && fromDescriptor.value.name === "from" &&
  ofDescriptor.value.length === 0 && ofDescriptor.value.name === "of";
numberOkay && bigintMapped[0] === 1n && bigintMapped[1] === 3n &&
  bigintCreated[0] === 3n && bigintCreated[1] === 4n && metadataOkay;
"#;

const OBSERVABLE_SOURCE: &str = r#"
var TypedArray = Object.getPrototypeOf(Int8Array);
var order = "";
var iterable = {};
iterable[Symbol.iterator] = function() {
  order += "i";
  var index = 0;
  return {
    next: function() {
      order += "n";
      if (index === 2) return { done: true };
      return { done: false, value: ++index };
    }
  };
};
function Result(length) {
  order += "c" + length;
  return new Uint8Array(length);
}
var mapped = TypedArray.from.call(Result, iterable, function(value, index) {
  order += "m" + index;
  return { valueOf: function() { order += "v" + index; return value + 10; } };
});
var iterableOkay = order === "innnc2m0v0m1v1" && mapped[0] === 11 && mapped[1] === 12;

order = "";
var arrayLike = { get length() { order += "l"; return 2; } };
Object.defineProperty(arrayLike, 0, { get: function() { order += "g0"; return 7; } });
Object.defineProperty(arrayLike, 1, { get: function() { order += "g1"; return 8; } });
var fromLike = TypedArray.from.call(Result, arrayLike);
var arrayLikeOkay = order === "lc2g0g1" && fromLike[0] === 7 && fromLike[1] === 8;

var custom = new Uint8Array(3);
var customResult = TypedArray.of.call(function(length) {
  order += "o" + length;
  return custom;
}, 9, 10);
var shortThrows = false;
try {
  TypedArray.of.call(function() { return new Uint8Array(1); }, 1, 2);
} catch (error) { shortThrows = error instanceof TypeError; }
var abrupt = {};
var abruptIdentity = false;
try { Uint8Array.from([1], function() { throw abrupt; }); }
catch (error) { abruptIdentity = error === abrupt; }
iterableOkay && arrayLikeOkay && customResult === custom && custom[0] === 9 && custom[1] === 10 &&
  shortThrows && abruptIdentity;
"#;

#[test]
fn typed_array_static_methods_work_for_every_dispatch_batch() {
    assert_typed_array_static::<1>(STATIC_SOURCE, false);
    assert_typed_array_static::<2>(STATIC_SOURCE, false);
    assert_typed_array_static::<4>(STATIC_SOURCE, false);
    assert_typed_array_static::<8>(STATIC_SOURCE, false);
    assert_typed_array_static::<16>(STATIC_SOURCE, false);
}

#[test]
fn typed_array_static_observable_order_survives_forced_major_collection() {
    assert_typed_array_static::<8>(OBSERVABLE_SOURCE, true);
}

/// Executes one TypedArray static-method fixture under a selected dispatch and GC policy.
fn assert_typed_array_static<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_typed_array_static_fixture(source);
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
                fuel: 8_000_000,
                quantum: 8_000_000,
            },
        )
        .expect("TypedArray static fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles one static-method fixture independently of VM scheduling policy.
fn compile_typed_array_static_fixture(source: &'static str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_437),
                SourceName::new("typed-array-static-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("TypedArray static fixture compiles")
}
