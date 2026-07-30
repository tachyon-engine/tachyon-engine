use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ITERATOR_INTRINSICS_SOURCE: &str = r#"
var iteratorPrototype = Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]()));
var globalDescriptor = Object.getOwnPropertyDescriptor(globalThis, "Iterator");
var constructorDescriptor = Object.getOwnPropertyDescriptor(iteratorPrototype, "constructor");
var tagDescriptor = Object.getOwnPropertyDescriptor(iteratorPrototype, Symbol.toStringTag);

var shapeOk = typeof Iterator === "function" && Iterator.name === "Iterator" &&
  Iterator.length === 0 && Iterator.prototype === iteratorPrototype &&
  globalDescriptor.value === Iterator && globalDescriptor.writable === true &&
  globalDescriptor.enumerable === false && globalDescriptor.configurable === true &&
  constructorDescriptor.get.name === "get constructor" &&
  constructorDescriptor.get.length === 0 &&
  constructorDescriptor.set.name === "set constructor" &&
  constructorDescriptor.set.length === 1 &&
  constructorDescriptor.enumerable === false && constructorDescriptor.configurable === true &&
  tagDescriptor.get.name === "get [Symbol.toStringTag]" &&
  tagDescriptor.get.length === 0 &&
  tagDescriptor.set.name === "set [Symbol.toStringTag]" &&
  tagDescriptor.set.length === 1 &&
  tagDescriptor.enumerable === false && tagDescriptor.configurable === true &&
  iteratorPrototype.constructor === Iterator &&
  constructorDescriptor.get.call() === Iterator &&
  constructorDescriptor.get.call({}) === Iterator &&
  tagDescriptor.get.call({}) === "Iterator" &&
  Object.prototype.toString.call(iteratorPrototype) === "[object Iterator]";

var callThrows = false;
var newThrows = false;
try { Iterator(); } catch (error) { callThrows = error instanceof TypeError; }
try { new Iterator(); } catch (error) { newThrows = error instanceof TypeError; }

class Derived extends Iterator {}
var derived = new Derived();
var subclassOk = Object.getPrototypeOf(derived) === Derived.prototype && derived instanceof Derived;
function NewTarget() {}
var customPrototype = {};
NewTarget.prototype = customPrototype;
var reflected = Reflect.construct(Iterator, [], NewTarget);
var reflectOk = Object.getPrototypeOf(reflected) === customPrototype;

var primitiveThrows = false;
var undefinedThrows = false;
var nullThrows = false;
var homeConstructorThrows = false;
var homeTagThrows = false;
var assignmentHomeThrows = false;
try { constructorDescriptor.set.call(1, 2); } catch (error) {
  primitiveThrows = error instanceof TypeError;
}
try { constructorDescriptor.set.call(undefined, 2); } catch (error) {
  undefinedThrows = error instanceof TypeError;
}
try { constructorDescriptor.set.call(null, 2); } catch (error) {
  nullThrows = error instanceof TypeError;
}
try { constructorDescriptor.set.call(iteratorPrototype, 2); } catch (error) {
  homeConstructorThrows = error instanceof TypeError;
}
try { iteratorPrototype.constructor = 2; } catch (error) {
  assignmentHomeThrows = error instanceof TypeError;
}
try { tagDescriptor.set.call(iteratorPrototype, 2); } catch (error) {
  homeTagThrows = error instanceof TypeError;
}

var child = Object.create(iteratorPrototype);
Object.freeze(iteratorPrototype);
child.constructor = 17;
var childDescriptor = Object.getOwnPropertyDescriptor(child, "constructor");
var inheritedCreateOk = childDescriptor.value === 17 && childDescriptor.writable === true &&
  childDescriptor.enumerable === true && childDescriptor.configurable === true;
child.constructor = { marker: 23 };
var existingUpdateOk = child.constructor.marker === 23;
var ordinary = { constructor: 29 };
constructorDescriptor.set.call(ordinary, 31);
var existingOrdinaryOk = ordinary.constructor === 31;
child[Symbol.toStringTag] = 41;
var childTagDescriptor = Object.getOwnPropertyDescriptor(child, Symbol.toStringTag);
var tagCreateOk = childTagDescriptor.value === 41 && childTagDescriptor.writable === true &&
  childTagDescriptor.enumerable === true && childTagDescriptor.configurable === true;

var defineTrace = "";
var defineTarget = {};
var defineProxy = new Proxy(defineTarget, {
  getOwnPropertyDescriptor: function(target, key) {
    defineTrace += "g";
    return Reflect.getOwnPropertyDescriptor(target, key);
  },
  defineProperty: function(target, key, descriptor) {
    defineTrace += "d";
    return Reflect.defineProperty(target, key, descriptor);
  }
});
constructorDescriptor.set.call(defineProxy, 53);
var proxyDefineOk = defineTrace === "gd" && defineTarget.constructor === 53;

var setTrace = "";
var setTarget = { constructor: 61 };
var setProxy = new Proxy(setTarget, {
  getOwnPropertyDescriptor: function(target, key) {
    setTrace += "g";
    return Reflect.getOwnPropertyDescriptor(target, key);
  },
  set: function(target, key, value, receiver) {
    setTrace += "s";
    target[key] = value;
    return true;
  }
});
constructorDescriptor.set.call(setProxy, 67);
var proxySetOk = setTrace === "gs" && setTarget.constructor === 67;

shapeOk && callThrows && newThrows && subclassOk && reflectOk && primitiveThrows &&
  undefinedThrows && nullThrows &&
  homeConstructorThrows && homeTagThrows && assignmentHomeThrows && inheritedCreateOk &&
  existingUpdateOk && existingOrdinaryOk && tagCreateOk && proxyDefineOk && proxySetOk;
"#;

const FOREIGN_NEW_TARGET_SOURCE: &str = r#"
function ForeignNewTarget() {}
ForeignNewTarget.prototype = undefined;
this.foreignNewTarget = ForeignNewTarget;
true;
"#;

const CROSS_REALM_FALLBACK_SOURCE: &str = r#"
var result = Reflect.construct(Iterator, [], foreignNewTarget);
Object.getPrototypeOf(result) === foreignIteratorPrototype;
"#;

const ITERATOR_FROM_SOURCE: &str = r#"
var fromDescriptor = Object.getOwnPropertyDescriptor(Iterator, "from");
var shapeOk = typeof Iterator.from === "function" && Iterator.from.name === "from" &&
  Iterator.from.length === 1 && fromDescriptor.writable === true &&
  fromDescriptor.enumerable === false && fromDescriptor.configurable === true;

var nextGets = 0;
var nextCalls = 0;
var returnGets = 0;
var base = {
  get next() {
    nextGets++;
    return function() {
      nextCalls++;
      return { value: 17, done: false, receiver: this === base };
    };
  },
  get return() {
    returnGets++;
    return function() { return { value: 23, done: true, receiver: this === base }; };
  }
};
var wrapper = Iterator.from(base);
var first = wrapper.next();
var returned = wrapper.return();
var wrapperOk = wrapper !== base && wrapper instanceof Iterator && nextGets === 1 &&
  nextCalls === 1 && first.value === 17 && first.receiver === true &&
  returnGets === 1 && returned.value === 23 && returned.receiver === true;

var noReturnBase = { next: function() { return { done: true }; } };
var noReturn = Iterator.from(noReturnBase).return();
var missingReturnOk = noReturn.value === undefined && noReturn.done === true &&
  Object.getPrototypeOf(noReturn) === Object.prototype;

var iteratorMethodCalls = 0;
var iterableNextGets = 0;
var yielded = { next: function() { return { done: true }; } };
var iterable = {
  [Symbol.iterator]: function() { iteratorMethodCalls++; return yielded; }
};
Object.defineProperty(yielded, "next", {
  configurable: true,
  get: function() { iterableNextGets++; return function() { return { done: true }; }; }
});
var iterableWrapper = Iterator.from(iterable);
var iterableOk = iteratorMethodCalls === 1 && iterableNextGets === 1 &&
  iterableWrapper !== yielded && iterableWrapper.next().done === true;

var ownNextGets = 0;
var alreadyIterator = Object.create(Iterator.prototype);
Object.defineProperty(alreadyIterator, "next", {
  get: function() { ownNextGets++; return function() { return { done: true }; }; }
});
var identityOk = Iterator.from(alreadyIterator) === alreadyIterator && ownNextGets === 1;

var invalidThis = false;
try { Object.getPrototypeOf(wrapper).next.call({}); } catch (error) {
  invalidThis = error instanceof TypeError;
}
var primitiveThrows = 0;
try { Iterator.from(undefined); } catch (error) { if (error instanceof TypeError) primitiveThrows++; }
try { Iterator.from(null); } catch (error) { if (error instanceof TypeError) primitiveThrows++; }
try { Iterator.from(1); } catch (error) { if (error instanceof TypeError) primitiveThrows++; }
var stringIterator = Iterator.from("ab");
var stringOk = stringIterator.next().value === "a" && stringIterator.next().value === "b";
var originalStringIterator = String.prototype[Symbol.iterator];
var observedStringReceiver = "";
Object.defineProperty(String.prototype, Symbol.iterator, {
  configurable: true,
  get: function() {
    "use strict";
    observedStringReceiver += typeof this;
    return originalStringIterator;
  }
});
Iterator.from("");
Iterator.from(new String(""));
var primitiveReceiverOk = observedStringReceiver === "stringobject";

shapeOk && wrapperOk && missingReturnOk && iterableOk && identityOk && invalidThis &&
  primitiveThrows === 3 && stringOk && primitiveReceiverOk;
"#;

#[test]
fn iterator_intrinsics_are_stable_for_every_dispatch_batch() {
    assert_iterator_intrinsics::<1>(8_901, false);
    assert_iterator_intrinsics::<2>(8_902, false);
    assert_iterator_intrinsics::<4>(8_904, false);
    assert_iterator_intrinsics::<8>(8_908, false);
    assert_iterator_intrinsics::<16>(8_916, false);
}

#[test]
fn iterator_intrinsic_roots_and_continuations_survive_forced_major_collection() {
    assert_iterator_intrinsics::<8>(8_920, true);
}

#[test]
fn iterator_constructor_fallback_uses_the_new_target_realm() {
    let child_module = compile_iterator_source(FOREIGN_NEW_TARGET_SOURCE, 8_930);
    let main_module = compile_iterator_source(CROSS_REALM_FALLBACK_SOURCE, 8_931);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let (child_realm, child_global) = isolate.create_realm().expect("child Realm initializes");
    let child_outcome = isolate
        .execute_in_realm(
            child_realm,
            &child_module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("child Realm fixture executes");
    assert!(
        matches!(child_outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );

    let new_target_atom = isolate.intern_intrinsic_name(b"foreignNewTarget").unwrap();
    let foreign_new_target = isolate
        .get_data_property(child_global, new_target_atom)
        .unwrap()
        .expect("child fixture publishes its constructor");
    let iterator_atom = isolate.intern_intrinsic_name(b"Iterator").unwrap();
    let child_iterator = isolate
        .get_data_property(child_global, iterator_atom)
        .unwrap()
        .expect("child Realm publishes Iterator");
    let prototype_atom = isolate.prototype_atom().unwrap();
    let foreign_iterator_prototype = isolate
        .get_data_property(child_iterator, prototype_atom)
        .unwrap()
        .expect("child Iterator publishes its prototype");
    publish_main_binding(&mut isolate, new_target_atom, foreign_new_target);
    let foreign_prototype_atom = isolate
        .intern_intrinsic_name(b"foreignIteratorPrototype")
        .unwrap();
    publish_main_binding(
        &mut isolate,
        foreign_prototype_atom,
        foreign_iterator_prototype,
    );

    let outcome = isolate
        .execute_with_batch::<8>(
            &main_module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("cross-Realm Iterator fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "cross-Realm Iterator fallback returned {outcome:?}"
    );
}

#[test]
fn iterator_from_and_wrapper_are_stable_for_every_dispatch_batch() {
    assert_iterator_from::<1>(8_941, false);
    assert_iterator_from::<2>(8_942, false);
    assert_iterator_from::<4>(8_944, false);
    assert_iterator_from::<8>(8_948, false);
    assert_iterator_from::<16>(8_956, false);
}

#[test]
fn iterator_from_roots_survive_forced_major_collection() {
    assert_iterator_from::<8>(8_960, true);
}

/// Compiles and executes the Iterator intrinsic graph under one dispatch and GC policy.
fn assert_iterator_intrinsics<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_iterator_intrinsics(source_id);
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
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("Iterator intrinsic fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(error) => isolate.native_error_kind(error).ok().flatten(),
        RunOutcome::Completed(_) | RunOutcome::BudgetExhausted => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Compiles the shared Iterator fixture independently from isolate collection policy.
fn compile_iterator_intrinsics(source_id: u32) -> CompiledModule {
    compile_iterator_source(ITERATOR_INTRINSICS_SOURCE, source_id)
}

/// Executes Iterator.from's full observable protocol under one dispatch and GC policy.
fn assert_iterator_from<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_iterator_source(ITERATOR_FROM_SOURCE, source_id);
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
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("Iterator.from fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(error) => isolate.native_error_kind(error).ok().flatten(),
        RunOutcome::Completed(_) | RunOutcome::BudgetExhausted => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Compiles one Iterator source fixture.
fn compile_iterator_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("iterator-intrinsics-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Iterator intrinsic fixture compiles")
}

/// Publishes one host-selected value through both global object and binding storage.
fn publish_main_binding(isolate: &mut Isolate, atom: AtomId, value: Value) {
    let global = isolate
        .realm
        .global_object
        .expect("main global initializes");
    isolate
        .set_own_data_property(global, atom, value)
        .expect("main global property publishes");
    isolate
        .realm
        .set(atom, value)
        .expect("main global binding publishes");
}
