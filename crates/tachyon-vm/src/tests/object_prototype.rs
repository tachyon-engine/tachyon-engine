use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const OBJECT_VALUE_OF_SOURCE: &str = r#"
var object = {};
var valueOf = Object.prototype.valueOf;
var descriptor = Object.getOwnPropertyDescriptor(Object.prototype, "valueOf");
var nullishThrows = false;
var constructThrows = false;
try { valueOf.call(null); } catch (error) { nullishThrows = error instanceof TypeError; }
try { new valueOf(); } catch (error) { constructThrows = error instanceof TypeError; }
valueOf.call(object) === object &&
valueOf.call(7).valueOf() === 7 &&
valueOf.call("wide").valueOf() === "wide" &&
valueOf.call(Symbol("s")).valueOf().description === "s" &&
typeof valueOf.call(true) === "object" &&
valueOf.call(true).valueOf() === true &&
new Boolean(false).valueOf() === false &&
new Boolean(true).toString() === "true" &&
Object(true).valueOf() === true &&
Object.prototype.toString.call(new Boolean(false)) === "[object Boolean]" &&
!Object.getOwnPropertyDescriptor(Boolean, "prototype").writable &&
valueOf.name === "valueOf" && valueOf.length === 0 &&
descriptor.value === valueOf && descriptor.writable &&
!descriptor.enumerable && descriptor.configurable &&
nullishThrows && constructThrows;
"#;

const OBJECT_TO_LOCALE_STRING_SOURCE: &str = r#"
"use strict";
var trace = "";
var locale = Object.prototype.toLocaleString;
var plain = { toString() { trace += "c"; return this === plain ? "plain" : "bad"; } };
var plainResult = locale.call(plain);
Object.defineProperty(Boolean.prototype, "toString", {
  configurable: true,
  get() {
    trace += "g";
    var kind = typeof this;
    return function() { trace += "b"; return kind + ":" + typeof this; };
  }
});
var primitiveResult = locale.call(true);
var proxy;
var target = { toString() { trace += "p"; return this === proxy ? "proxy" : "bad"; } };
proxy = new Proxy(target, {
  get(target, key, receiver) { trace += "x"; return Reflect.get(target, key, receiver); }
});
var proxyResult = locale.call(proxy);
var nonCallableThrows = false;
try { locale.call({ toString: 1 }); } catch (error) { nonCallableThrows = error instanceof TypeError; }
var marker = {};
var abrupt = false;
try { locale.call({ get toString() { throw marker; } }); } catch (error) { abrupt = error === marker; }
plainResult === "plain" && primitiveResult === "boolean:boolean" &&
proxyResult === "proxy" && trace === "cgbxp" &&
nonCallableThrows && abrupt && locale.name === "toLocaleString" && locale.length === 0;
"#;

const OBJECT_IS_PROTOTYPE_OF_SOURCE: &str = r#"
function A() {}
function B() {}
var proto = new A();
B.prototype = proto;
var value = new B();
var trapGets = 0;
var trapCalls = 0;
var proxyProto = {};
var handler = {
  get getPrototypeOf() {
    trapGets += 1;
    return function(target) { trapCalls += 1; return proxyProto; };
  }
};
var proxy = new Proxy({}, handler);
var nullishThrows = false;
try { Object.prototype.isPrototypeOf.call(null, value); }
catch (error) { nullishThrows = error instanceof TypeError; }
var primitiveArgumentSkipsReceiver = Object.prototype.isPrototypeOf.call(null, 1) === false;
proto.isPrototypeOf(value) && A.prototype.isPrototypeOf(value) &&
!Number.isPrototypeOf(value) && proxyProto.isPrototypeOf(proxy) &&
!Object.prototype.isPrototypeOf.call(true, proxy) &&
trapGets === 2 && trapCalls === 2 && nullishThrows && primitiveArgumentSkipsReceiver;
"#;

const OBJECT_DEFINE_LEGACY_ACCESSOR_SOURCE: &str = r#"
var trace = "";
var object = {};
var key = {
  [Symbol.toPrimitive]() { trace += "k"; return "value"; }
};
function getter() { return this === object ? 41 : -1; }
function setter(value) { this.seen = value; }
var first = object.__defineGetter__(key, getter);
var second = object.__defineSetter__("value", setter);
var descriptor = Object.getOwnPropertyDescriptor(object, "value");
var read = object.value;
object.value = 9;
var proxy;
var target = {};
proxy = new Proxy(target, {
  defineProperty(target, key, descriptor) {
    trace += "p";
    return Reflect.defineProperty(target, key, descriptor);
  }
});
function proxyGetter() { return this === proxy ? 7 : -1; }
proxy.__defineGetter__("answer", proxyGetter);
first === undefined && second === undefined && read === 41 && object.seen === 9 &&
descriptor.get === getter && descriptor.set === setter && descriptor.enumerable &&
descriptor.configurable && proxy.answer === 7 && trace === "kp";
"#;

const OBJECT_LOOKUP_LEGACY_ACCESSOR_SOURCE: &str = r#"
var trace = "";
function getter() { return 1; }
function setter(value) {}
var root = {};
root.__defineGetter__("value", getter);
root.__defineSetter__("value", setter);
var proxy = new Proxy({}, {
  getOwnPropertyDescriptor(target, key) { trace += "o"; return undefined; },
  getPrototypeOf(target) { trace += "p"; return root; }
});
var subject = Object.create(proxy);
var key = { [Symbol.toPrimitive]() { trace += "k"; return "value"; } };
var foundGetter = subject.__lookupGetter__(key);
var foundSetter = subject.__lookupSetter__("value");
var shadow = Object.create(root);
Object.defineProperty(shadow, "value", { value: 1 });
foundGetter === getter && foundSetter === setter &&
shadow.__lookupGetter__("value") === undefined &&
shadow.__lookupSetter__("value") === undefined && trace === "kopop";
"#;

const OBJECT_PROTO_ACCESSOR_SOURCE: &str = r#"
var trace = "";
var descriptor = Object.getOwnPropertyDescriptor(Object.prototype, "__proto__");
var first = {};
var second = {};
var target = Object.create(first);
var proxy = new Proxy(target, {
  getPrototypeOf(target) { trace += "g"; return Reflect.getPrototypeOf(target); },
  setPrototypeOf(target, prototype) {
    trace += "s";
    return Reflect.setPrototypeOf(target, prototype);
  }
});
var before = descriptor.get.call(proxy);
var setResult = descriptor.set.call(proxy, second);
var after = descriptor.get.call(proxy);
proxy.__proto__ = first;
var afterAssignment = descriptor.get.call(proxy);
var ignored = descriptor.set.call(proxy, 1);
var marker = {};
var abruptProxy = new Proxy({}, { get set() { throw marker; } });
var abrupt = false;
try { abruptProxy.__proto__ = second; } catch (error) { abrupt = error === marker; }
var frozenTarget = {};
Object.defineProperty(frozenTarget, "locked", {
  value: 1, writable: false, configurable: false
});
var frozenProxy = new Proxy(frozenTarget, { set() { return true; } });
var invariantThrows = false;
try { frozenProxy.locked = 2; } catch (error) { invariantThrows = error instanceof TypeError; }
var observedReceiver;
var prototypeProxy = new Proxy({}, {
  set(target, key, value, receiver) { observedReceiver = receiver; return true; }
});
var inheritedReceiver = Object.create(prototypeProxy);
inheritedReceiver.value = 3;
var receiverTrace = "";
var receiverTarget = { value: 1 };
var receiverProxy = new Proxy(receiverTarget, {
  getOwnPropertyDescriptor(target, key) {
    receiverTrace += "g";
    return Reflect.getOwnPropertyDescriptor(target, key);
  },
  defineProperty(target, key, descriptor) {
    receiverTrace += "d";
    return Reflect.defineProperty(target, key, descriptor);
  }
});
receiverProxy.value = 2;
var array = [1, 2];
var arrayProxy = new Proxy(new Proxy(array, {}), { set: null });
arrayProxy.length = 0;
Object.preventExtensions(array);
var arrayRejectsIndex = !Reflect.set(arrayProxy, "0", 3);
var string = new String("abc");
var stringProxy = new Proxy(new Proxy(string, {}), { set: null });
stringProxy[4] = 9;
var functionTarget = function() {};
var functionInnerProxy = new Proxy(functionTarget, {});
var functionProxy = new Proxy(functionInnerProxy, { set: undefined });
var functionSet = Reflect.set(functionProxy, "prototype", null);
var ownKeysProxy = new Proxy({ a: 1 }, {
  ownKeys(target) { return ["a"]; }
});
var ownKeysResult = Reflect.ownKeys(ownKeysProxy);
var indexedReceiver;
var indexedProxy = new Proxy({}, {
  set(target, key, value, receiver) {
    indexedReceiver = receiver;
    return key === "0" && value === 1;
  }
});
var indexedArray = new Array(1);
Object.setPrototypeOf(indexedArray, indexedProxy);
indexedArray[0] = 1;
var root = {};
var intermediary = Object.create(root);
var leaf = Object.create(intermediary);
var cycleThrows = false;
try { descriptor.set.call(root, leaf); } catch (error) { cycleThrows = error instanceof TypeError; }
before === first && after === second && afterAssignment === first &&
setResult === undefined && ignored === undefined && trace === "gsgsg" &&
abrupt && invariantThrows && observedReceiver === inheritedReceiver &&
receiverTrace === "gd" && receiverTarget.value === 2 &&
array.length === 0 && arrayRejectsIndex &&
string[4] === 9 && functionSet && functionTarget.prototype === null &&
indexedReceiver === indexedArray && !Object.hasOwn(indexedArray, "0") &&
ownKeysResult.length === 1 && ownKeysResult[0] === "a" &&
cycleThrows && Object.getPrototypeOf(root) === Object.prototype &&
descriptor.get.name === "get __proto__" && descriptor.set.name === "set __proto__";
"#;

#[test]
fn object_value_of_executes_for_every_dispatch_batch() {
    assert_object_value_of_batch::<1>();
    assert_object_value_of_batch::<2>();
    assert_object_value_of_batch::<4>();
    assert_object_value_of_batch::<8>();
    assert_object_value_of_batch::<16>();
}

#[test]
/// Forces collection through every primitive wrapper allocation in the Object ToObject path.
fn object_value_of_boxing_survives_forced_major_collections() {
    let module = compile_object_value_of_source(106);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("forced-major Object.prototype.valueOf fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn object_to_locale_string_resumes_for_every_dispatch_batch() {
    assert_to_locale_string_batch::<1>();
    assert_to_locale_string_batch::<2>();
    assert_to_locale_string_batch::<4>();
    assert_to_locale_string_batch::<8>();
    assert_to_locale_string_batch::<16>();
}

#[test]
/// Forces collection through nested getter, Proxy, and method-call continuations.
fn object_to_locale_string_survives_forced_major_collections() {
    let module = compile_object_source(OBJECT_TO_LOCALE_STRING_SOURCE, 116);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major Object.prototype.toLocaleString fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn object_is_prototype_of_resumes_for_every_dispatch_batch() {
    assert_is_prototype_of_batch::<1>();
    assert_is_prototype_of_batch::<2>();
    assert_is_prototype_of_batch::<4>();
    assert_is_prototype_of_batch::<8>();
    assert_is_prototype_of_batch::<16>();
}

#[test]
/// Forces collection while the Proxy trap getter and returned trap function are suspended.
fn object_is_prototype_of_survives_forced_major_collections() {
    let module = compile_object_source(OBJECT_IS_PROTOTYPE_OF_SOURCE, 122);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major Object.prototype.isPrototypeOf fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn object_define_legacy_accessor_resumes_for_every_dispatch_batch() {
    assert_define_legacy_accessor_batch::<1>();
    assert_define_legacy_accessor_batch::<2>();
    assert_define_legacy_accessor_batch::<4>();
    assert_define_legacy_accessor_batch::<8>();
    assert_define_legacy_accessor_batch::<16>();
}

#[test]
/// Forces collection during key conversion, Proxy trap invocation, and accessor publication.
fn object_define_legacy_accessor_survives_forced_major_collections() {
    let module = compile_object_source(OBJECT_DEFINE_LEGACY_ACCESSOR_SOURCE, 142);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 12_288,
                quantum: 12_288,
            },
        )
        .expect("forced-major legacy accessor fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn object_lookup_legacy_accessor_resumes_for_every_dispatch_batch() {
    assert_lookup_legacy_accessor_batch::<1>();
    assert_lookup_legacy_accessor_batch::<2>();
    assert_lookup_legacy_accessor_batch::<4>();
    assert_lookup_legacy_accessor_batch::<8>();
    assert_lookup_legacy_accessor_batch::<16>();
}

#[test]
/// Forces collection across Proxy get-own/get-prototype callbacks and key conversion.
fn object_lookup_legacy_accessor_survives_forced_major_collections() {
    let module = compile_object_source(OBJECT_LOOKUP_LEGACY_ACCESSOR_SOURCE, 162);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("forced-major legacy accessor lookup fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn object_proto_accessor_resumes_for_every_dispatch_batch() {
    assert_object_proto_accessor_batch::<1>();
    assert_object_proto_accessor_batch::<2>();
    assert_object_proto_accessor_batch::<4>();
    assert_object_proto_accessor_batch::<8>();
    assert_object_proto_accessor_batch::<16>();
}

#[test]
/// Forces collection through both Proxy prototype traps invoked by the legacy accessor pair.
fn object_proto_accessor_survives_forced_major_collections() {
    let module = compile_object_source(OBJECT_PROTO_ACCESSOR_SOURCE, 182);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("forced-major Object.prototype.__proto__ fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Executes the Object.prototype.valueOf contract with one selected dispatch monomorphization.
fn assert_object_value_of_batch<const N: usize>() {
    let module = compile_object_value_of_source(100 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("Object.prototype.valueOf fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_object_value_of_source(source_id: u32) -> CompiledModule {
    compile_object_source(OBJECT_VALUE_OF_SOURCE, source_id)
}

/// Executes the observable Get/Call sequence with one selected dispatch monomorphization.
fn assert_to_locale_string_batch<const N: usize>() {
    let module = compile_object_source(OBJECT_TO_LOCALE_STRING_SOURCE, 110 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("Object.prototype.toLocaleString fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes ordinary and Proxy prototype walks with one dispatch monomorphization.
fn assert_is_prototype_of_batch<const N: usize>() {
    let module = compile_object_source(OBJECT_IS_PROTOTYPE_OF_SOURCE, 120 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("Object.prototype.isPrototypeOf fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes both legacy accessor definitions with one dispatch monomorphization.
fn assert_define_legacy_accessor_batch<const N: usize>() {
    let module = compile_object_source(OBJECT_DEFINE_LEGACY_ACCESSOR_SOURCE, 140 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 12_288,
                quantum: 12_288,
            },
        )
        .expect("legacy accessor fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes legacy getter/setter lookup through one Proxy prototype with one batch size.
fn assert_lookup_legacy_accessor_batch<const N: usize>() {
    let module = compile_object_source(OBJECT_LOOKUP_LEGACY_ACCESSOR_SOURCE, 160 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("legacy accessor lookup fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes the legacy prototype accessor pair with one dispatch monomorphization.
fn assert_object_proto_accessor_batch<const N: usize>() {
    let module = compile_object_source(OBJECT_PROTO_ACCESSOR_SOURCE, 180 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("Object.prototype.__proto__ fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_object_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("object-prototype"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Object.prototype fixture compiles")
}
