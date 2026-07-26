use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_BUFFER_SOURCE: &str = r#"
var b = new ArrayBuffer(8);
b.byteLength === 8 && b.maxByteLength === 8 && !b.resizable && !b.detached &&
  ArrayBuffer.isView(b) === false && Object.getPrototypeOf(b) === ArrayBuffer.prototype &&
  ArrayBuffer.prototype.constructor === ArrayBuffer &&
  Object.prototype.toString.call(b) === "[object ArrayBuffer]";
"#;

const ARRAY_BUFFER_DETACH_SOURCE: &str = r#"
var buffer = new ArrayBuffer(16);
var typed = new Uint8Array(buffer, 4, 4);
var view = new DataView(buffer, 2, 8);
typed[0] = 23;
var detachResult = $262.detachArrayBuffer(buffer);
var dataViewLengthThrows = false;
var dataViewOffsetThrows = false;
var dataViewReadThrows = false;
var dataViewIndexThrowsFirst = false;
var constructorIndexThrowsFirst = false;
try { view.byteLength; } catch (error) { dataViewLengthThrows = error instanceof TypeError; }
try { view.byteOffset; } catch (error) { dataViewOffsetThrows = error instanceof TypeError; }
try { view.getUint8(13); } catch (error) { dataViewReadThrows = error instanceof TypeError; }
try { view.getUint8(Infinity); } catch (error) { dataViewIndexThrowsFirst = error instanceof RangeError; }
try { new DataView(buffer, Infinity); } catch (error) {
  constructorIndexThrowsFirst = error instanceof RangeError;
}
var invalidThrows = false;
try { $262.detachArrayBuffer({}); } catch (error) { invalidThrows = error instanceof TypeError; }
$262.detachArrayBuffer(buffer);

var duringAtBuffer = new ArrayBuffer(4);
var duringAt = new Uint8Array(duringAtBuffer);
duringAt[0] = 7;
var atResult = duringAt.at({
  valueOf: function() { $262.detachArrayBuffer(duringAtBuffer); return 0; }
});
var duringIncludesBuffer = new ArrayBuffer(4);
var duringIncludes = new Uint8Array(duringIncludesBuffer);
var includesResult = duringIncludes.includes(undefined, {
  valueOf: function() { $262.detachArrayBuffer(duringIncludesBuffer); return 0; }
});

detachResult === undefined && buffer.detached && buffer.byteLength === 0 &&
buffer.maxByteLength === 0 && buffer.resizable === false &&
typed.buffer === buffer && typed.length === 0 && typed.byteLength === 0 &&
typed.byteOffset === 0 && typed[0] === undefined &&
view.buffer === buffer && dataViewLengthThrows && dataViewOffsetThrows &&
dataViewReadThrows && dataViewIndexThrowsFirst && constructorIndexThrowsFirst &&
invalidThrows && atResult === undefined && includesResult === true;
"#;

const ARRAY_BUFFER_SLICE_SOURCE: &str = r#"
var source = new ArrayBuffer(6);
var sourceBytes = new Uint8Array(source);
sourceBytes[0] = 10;
sourceBytes[1] = 11;
sourceBytes[2] = 12;
sourceBytes[3] = 13;
sourceBytes[4] = 14;
sourceBytes[5] = 15;
var order = "";
var holder = {};
Object.defineProperty(holder, Symbol.species, {
  get: function() {
    order += "p";
    return function Species(length) {
      order += "c" + length;
      return new ArrayBuffer(length + 1);
    };
  }
});
source.constructor = holder;
var result = source.slice(
  { valueOf: function() { order += "s"; return -5; } },
  { valueOf: function() { order += "e"; return 5; } }
);
var resultBytes = new Uint8Array(result);
var basic = order === "sepc4" && result.byteLength === 5 &&
  resultBytes[0] === 11 && resultBytes[1] === 12 && resultBytes[2] === 13 &&
  resultBytes[3] === 14 && resultBytes[4] === 0;

var infinity = source.slice(-Infinity, Infinity);
var infinityBytes = new Uint8Array(infinity);
var infinityOk = infinity.byteLength === 7 && infinityBytes[0] === 10 &&
  infinityBytes[5] === 15 && infinityBytes[6] === 0;

var aliasSource = new ArrayBuffer(2);
var aliasHolder = {};
aliasHolder[Symbol.species] = function() { return aliasSource; };
aliasSource.constructor = aliasHolder;
var aliasThrows = false;
try { aliasSource.slice(0); } catch (error) { aliasThrows = error instanceof TypeError; }

var detachedResultSource = new ArrayBuffer(2);
var detachedResultHolder = {};
detachedResultHolder[Symbol.species] = function(length) {
  var value = new ArrayBuffer(length);
  $262.detachArrayBuffer(value);
  return value;
};
detachedResultSource.constructor = detachedResultHolder;
var detachedResultThrows = false;
try { detachedResultSource.slice(0); } catch (error) {
  detachedResultThrows = error instanceof TypeError;
}

var smallSource = new ArrayBuffer(3);
var smallHolder = {};
smallHolder[Symbol.species] = function() { return new ArrayBuffer(1); };
smallSource.constructor = smallHolder;
var smallThrows = false;
try { smallSource.slice(0); } catch (error) { smallThrows = error instanceof TypeError; }

var detachSource = new ArrayBuffer(3);
var detachHolder = {};
detachHolder[Symbol.species] = function(length) {
  $262.detachArrayBuffer(detachSource);
  return new ArrayBuffer(length);
};
detachSource.constructor = detachHolder;
var detachSourceThrows = false;
try { detachSource.slice(0); } catch (error) {
  detachSourceThrows = error instanceof TypeError;
}

var nonConstructorSource = new ArrayBuffer(1);
var nonConstructorHolder = {};
nonConstructorHolder[Symbol.species] = {};
nonConstructorSource.constructor = nonConstructorHolder;
var nonConstructorThrows = false;
try { nonConstructorSource.slice(0); } catch (error) {
  nonConstructorThrows = error instanceof TypeError;
}

basic && infinityOk && aliasThrows && detachedResultThrows && smallThrows &&
detachSourceThrows && nonConstructorThrows;
"#;

const ARRAY_BUFFER_SLICE_CROSS_REALM_SOURCE: &str = r#"
var source = new ArrayBuffer(4);
var bytes = new Uint8Array(source);
bytes[1] = 37;
source.constructor = foreignArrayBuffer;
var result = source.slice(1, 2);
globalThis.crossRealmResult = result;
result.byteLength === 1 && new Uint8Array(result)[0] === 37 &&
  true;
"#;

const ARRAY_BUFFER_SLICE_FORCED_MAJOR_SOURCE: &str = r#"
var source = new ArrayBuffer(4);
var bytes = new Uint8Array(source);
bytes[1] = 41;
bytes[2] = 42;
var holder = {};
holder[Symbol.species] = function(length) { return new ArrayBuffer(length); };
source.constructor = holder;
var result = source.slice(
  { valueOf: function() { return 1; } },
  { valueOf: function() { return 3; } }
);
var resultBytes = new Uint8Array(result);
result.byteLength === 2 && resultBytes[0] === 41 && resultBytes[1] === 42;
"#;

#[test]
fn array_buffer_fixed_constructor_and_accessors_work_for_dispatch_batches() {
    assert_array_buffer_source::<1>();
    assert_array_buffer_source::<2>();
    assert_array_buffer_source::<4>();
    assert_array_buffer_source::<8>();
    assert_array_buffer_source::<16>();
}

#[test]
fn array_buffer_backing_survives_forced_major_collection() {
    let module = compile_array_buffer_fixture();
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("ArrayBuffer fixture survives forced major GC");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

#[test]
fn detach_is_observed_by_every_fixed_view_for_dispatch_batches() {
    assert_array_buffer_detach::<1>(false);
    assert_array_buffer_detach::<2>(false);
    assert_array_buffer_detach::<4>(false);
    assert_array_buffer_detach::<8>(false);
    assert_array_buffer_detach::<16>(false);
}

#[test]
fn detach_edges_survive_forced_major_collection() {
    assert_array_buffer_detach::<8>(true);
}

#[test]
fn array_buffer_slice_observes_species_and_validation_order_for_dispatch_batches() {
    assert_array_buffer_slice::<1>(false);
    assert_array_buffer_slice::<2>(false);
    assert_array_buffer_slice::<4>(false);
    assert_array_buffer_slice::<8>(false);
    assert_array_buffer_slice::<16>(false);
}

#[test]
fn array_buffer_slice_state_and_copy_survive_forced_major_collection() {
    assert_array_buffer_slice::<8>(true);
}

#[test]
fn array_buffer_slice_constructs_foreign_species_in_its_realm() {
    let module = compile_source(ARRAY_BUFFER_SLICE_CROSS_REALM_SOURCE, 7_413);
    let mut isolate = test_isolate();
    let (_, child_global) = isolate.create_realm().expect("child Realm initializes");
    let constructor_atom = isolate.intern_intrinsic_name(b"ArrayBuffer").unwrap();
    let foreign_constructor = isolate
        .get_data_property(child_global, constructor_atom)
        .unwrap()
        .expect("child Realm publishes ArrayBuffer");
    let prototype_atom = isolate.intern_intrinsic_name(b"prototype").unwrap();
    let foreign_prototype = isolate
        .get_data_property(foreign_constructor, prototype_atom)
        .unwrap()
        .expect("foreign ArrayBuffer constructor publishes prototype");
    let foreign_atom = isolate
        .intern_intrinsic_name(b"foreignArrayBuffer")
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
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .expect("cross-Realm ArrayBuffer slice executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "cross-Realm ArrayBuffer slice returned {outcome:?}"
    );
    let result_atom = isolate.intern_intrinsic_name(b"crossRealmResult").unwrap();
    let result = isolate
        .get_data_property(global, result_atom)
        .unwrap()
        .expect("fixture publishes cross-Realm result");
    assert_eq!(
        isolate.object_prototype_of(result).unwrap(),
        foreign_prototype,
        "foreign ArrayBuffer species must use its constructor Realm prototype"
    );
}

/// Compiles and runs the fixed ArrayBuffer fixture under one dispatch policy.
fn assert_array_buffer_source<const N: usize>() {
    let module = compile_array_buffer_fixture();
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("ArrayBuffer fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes the host detach, view observation, ordering, and idempotence fixture.
fn assert_array_buffer_detach<const N: usize>(forced_major: bool) {
    let module = compile_source(ARRAY_BUFFER_DETACH_SOURCE, 7_411);
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
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .expect("ArrayBuffer detach fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes observable index conversion, species construction, validation, and copy ordering.
fn assert_array_buffer_slice<const N: usize>(forced_major: bool) {
    let source = if forced_major {
        ARRAY_BUFFER_SLICE_FORCED_MAJOR_SOURCE
    } else {
        ARRAY_BUFFER_SLICE_SOURCE
    };
    let module = compile_source(source, 7_412);
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
        .expect("ArrayBuffer slice fixture executes");
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

/// Compiles the shared fixture independently of dispatch and collection policy.
fn compile_array_buffer_fixture() -> CompiledModule {
    compile_source(ARRAY_BUFFER_SOURCE, 7_410)
}

/// Compiles one ArrayBuffer fixture independently of dispatch and collection policy.
fn compile_source(source: &'static str, id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(id),
                SourceName::new("array-buffer-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("ArrayBuffer fixture compiles")
}
use std::sync::Arc;
