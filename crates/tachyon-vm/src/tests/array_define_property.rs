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

var assignmentCalls = 0;
var assignmentTarget = [0, 1, 2, 3];
var assignmentValue = {
  valueOf: function() { assignmentCalls = assignmentCalls + 1; return 2; }
};
var assignmentResult = (assignmentTarget.length = assignmentValue);
var assignmentConversion = assignmentResult === assignmentValue &&
  assignmentCalls === 2 && assignmentTarget.length === 2 && !(2 in assignmentTarget);

var reflectCalls = 0;
var reflectTarget = [0, 1, 2];
var reflectResult = Reflect.set(reflectTarget, "length", {
  valueOf: function() { reflectCalls = reflectCalls + 1; return 1; }
});
var reflectConversion = reflectResult === true && reflectCalls === 2 &&
  reflectTarget.length === 1 && !(1 in reflectTarget);

var assignCalls = 0;
var assignTarget = [0, 1, 2];
var assignResult = Object.assign(assignTarget, {
  length: { valueOf: function() { assignCalls = assignCalls + 1; return 1; } }
});
var objectAssignConversion = assignResult === assignTarget && assignCalls === 2 &&
  assignTarget.length === 1 && !(1 in assignTarget);

var mutationCalls = 0;
var mutationTarget = [0, 1, 2];
var mutationResult = Reflect.set(mutationTarget, "length", {
  valueOf: function() {
    mutationCalls = mutationCalls + 1;
    if (mutationCalls === 1) {
      Object.defineProperty(mutationTarget, "length", { writable: false });
    }
    return 1;
  }
});
var conversionMutation = mutationResult === false && mutationCalls === 2 &&
  mutationTarget.length === 3 &&
  Object.getOwnPropertyDescriptor(mutationTarget, "length").writable === false;

var preblockedCalls = 0;
var preblockedTarget = [0, 1];
Object.defineProperty(preblockedTarget, "length", { writable: false });
var preblockedValue = {
  valueOf: function() { preblockedCalls = preblockedCalls + 1; return 1; }
};
preblockedTarget.length = preblockedValue;
var preblocked = preblockedCalls === 0 &&
  Reflect.set(preblockedTarget, "length", preblockedValue) === false &&
  preblockedCalls === 0 && preblockedTarget.length === 2;

primitives && invalid && observableConversion && mismatchCheck &&
  definePropertiesConversion && flagChecks && shrink && rollback &&
  noPublish && belowLength && grew && assignmentConversion && reflectConversion &&
  objectAssignConversion && conversionMutation && preblocked;
"#;

const ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE: &str = r#"
var calls = 0;
var target = [0, 1, 2];
var proxy = new Proxy(target, {});
var value = { valueOf: function() { calls = calls + 1; return 1; } };
var result = (proxy.length = value);
result === value && calls === 2 && target.length === 1;
"#;

const ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE: &str = r#"
var calls = 0;
var target = [0, 1, 2];
var proxy = new Proxy(target, {});
var result = Object.assign(proxy, {
  length: { valueOf: function() { calls = calls + 1; return 1; } }
});
result === proxy && calls === 2 && target.length === 1;
"#;

#[test]
fn array_define_property_is_stable_for_every_dispatch_batch() {
    assert_array_define_property::<1>(2_101, false, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<2>(2_102, false, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<4>(2_104, false, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<8>(2_108, false, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<16>(2_116, false, ARRAY_DEFINE_PROPERTY_SOURCE);
}

#[test]
fn array_define_property_survives_forced_major_collections() {
    assert_array_define_property::<1>(2_121, true, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<2>(2_122, true, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<4>(2_124, true, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<8>(2_128, true, ARRAY_DEFINE_PROPERTY_SOURCE);
    assert_array_define_property::<16>(2_136, true, ARRAY_DEFINE_PROPERTY_SOURCE);
}

#[test]
fn array_length_proxy_assignment_uses_the_exotic_receiver_define() {
    assert_array_define_property::<1>(2_141, false, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<2>(2_142, false, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<4>(2_144, false, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<8>(2_148, false, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<16>(2_156, false, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<1>(2_161, true, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<2>(2_162, true, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<4>(2_164, true, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<8>(2_168, true, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
    assert_array_define_property::<16>(2_176, true, ARRAY_LENGTH_PROXY_ASSIGNMENT_SOURCE);
}

#[test]
fn array_length_proxy_object_assign_resumes_the_copy_cursor() {
    assert_array_define_property::<1>(2_181, false, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<2>(2_182, false, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<4>(2_184, false, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<8>(2_188, false, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<16>(2_196, false, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<1>(2_201, true, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<2>(2_202, true, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<4>(2_204, true, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<8>(2_208, true, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
    assert_array_define_property::<16>(2_216, true, ARRAY_LENGTH_PROXY_OBJECT_ASSIGN_SOURCE);
}

/// Compiles and executes the Array descriptor fixture under one VM dispatch/GC policy.
fn assert_array_define_property<const N: usize>(
    source_id: u32,
    forced_major: bool,
    source: &'static str,
) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-define-property-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
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
