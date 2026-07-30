use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_DEFINE_PROPERTY_SOURCE: &str = r#"
function lengthIs(value, expected) {
  var array = [0, 1, 2, 3];
  Object.defineProperty(array, "length", { value: value });
  return array.length === expected;
}
function rejectsLength(value) {
  try {
    Object.defineProperty([], "length", { value: value });
    return false;
  } catch (error) {
    return error instanceof RangeError;
  }
}

var primitives = lengthIs(null, 0) && lengthIs(false, 0) &&
  lengthIs(true, 1) && lengthIs("3", 3);
var invalid = rejectsLength(3.5) && rejectsLength(-1) &&
  rejectsLength(NaN) && rejectsLength(Infinity);

var conversionCalls = 0;
var converted = [0, 1, 2, 3];
Object.defineProperty(converted, "length", {
  value: { valueOf: function() { conversionCalls = conversionCalls + 1; return 2; } }
});
var observableConversion = conversionCalls === 2 && converted.length === 2 && !(2 in converted);

var mismatchCalls = 0;
var mismatch = [0, 1, 2, 3];
var mismatchRejected = false;
try {
  Object.defineProperty(mismatch, "length", {
    value: { valueOf: function() { mismatchCalls = mismatchCalls + 1; return mismatchCalls + 1; } }
  });
} catch (error) {
  mismatchRejected = error instanceof RangeError;
}
var mismatchCheck = mismatchRejected && mismatchCalls === 2 && mismatch.length === 4;

var definePropertiesCalls = 0;
var definePropertiesTarget = [0, 1, 2];
Object.defineProperties(definePropertiesTarget, {
  length: { value: {
    valueOf: function() { definePropertiesCalls = definePropertiesCalls + 1; return 1; }
  }}
});
var definePropertiesConversion = definePropertiesCalls === 2 &&
  definePropertiesTarget.length === 1 && !(1 in definePropertiesTarget);

var flags = [];
var flagChecks = !Reflect.defineProperty(flags, "length", { enumerable: true }) &&
  !Reflect.defineProperty(flags, "length", { configurable: true }) &&
  !Reflect.defineProperty(flags, "length", { get: function() { return 0; } });
Object.defineProperty(flags, "length", { writable: false });
flagChecks = flagChecks &&
  !Reflect.defineProperty(flags, "length", { writable: true }) &&
  Reflect.defineProperty(flags, "length", { value: 0 });

var mixed = [0, 1, 2, 3, 4];
Object.defineProperty(mixed, "8", {
  value: 8, writable: false, enumerable: false, configurable: true
});
Object.defineProperty(mixed, "length", { value: 2 });
var shrink = mixed.length === 2 && !(2 in mixed) && !(4 in mixed) && !(8 in mixed);

var blocked = [0, 1, 2, 3, 4, 5];
Object.defineProperty(blocked, "3", { configurable: false });
var blockedResult = Reflect.defineProperty(blocked, "length", {
  value: 1, writable: false
});
var blockedDescriptor = Object.getOwnPropertyDescriptor(blocked, "length");
var rollback = blockedResult === false && blocked.length === 4 &&
  blockedDescriptor.writable === false && !(5 in blocked) && !(4 in blocked) &&
  (3 in blocked) && (2 in blocked);

var frozenLength = [10];
Object.defineProperty(frozenLength, "length", { writable: false });
var noPublish = !Reflect.defineProperty(frozenLength, "1", {
  value: 20, writable: true, enumerable: true, configurable: true
}) && !(1 in frozenLength) && frozenLength.length === 1;
var belowLength = Reflect.defineProperty(frozenLength, "0", { value: 11 }) &&
  frozenLength[0] === 11 && frozenLength.length === 1;

var growth = [];
Object.defineProperty(growth, "4", {
  value: 4, writable: false, enumerable: false, configurable: false
});
var grew = growth.length === 5 && growth[4] === 4;

primitives && invalid && observableConversion && mismatchCheck &&
  definePropertiesConversion && flagChecks && shrink && rollback &&
  noPublish && belowLength && grew;
"#;

#[test]
fn array_define_property_is_stable_for_every_dispatch_batch() {
    assert_array_define_property::<1>(2_101, false);
    assert_array_define_property::<2>(2_102, false);
    assert_array_define_property::<4>(2_104, false);
    assert_array_define_property::<8>(2_108, false);
    assert_array_define_property::<16>(2_116, false);
}

#[test]
fn array_define_property_survives_forced_major_collections() {
    assert_array_define_property::<1>(2_121, true);
    assert_array_define_property::<2>(2_122, true);
    assert_array_define_property::<4>(2_124, true);
    assert_array_define_property::<8>(2_128, true);
    assert_array_define_property::<16>(2_136, true);
}

/// Compiles and executes the Array descriptor fixture under one VM dispatch/GC policy.
fn assert_array_define_property<const N: usize>(source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-define-property-fixture"),
                MediaType::JavaScript,
                Arc::from(ARRAY_DEFINE_PROPERTY_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Array defineProperty fixture compiles");
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
        .expect("Array defineProperty fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
