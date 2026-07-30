use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

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

const ITERATOR_MAP_SOURCE: &str = r#"
var invalidClosed = 0;
var invalidTypeError = false;
var invalid = {
  __proto__: Iterator.prototype,
  get next() { throw new Error("next must not be read"); },
  return() { invalidClosed++; return {}; }
};
try { invalid.map(); } catch (error) { invalidTypeError = error instanceof TypeError; }
var invalidObjectTypeError = false;
function invokeInvalidMap() { invalid.map({}); }
try { invokeInvalidMap(); } catch (error) { invalidObjectTypeError = error instanceof TypeError; }

var nextGets = 0;
var nextCalls = 0;
var returnCalls = 0;
var cursor = 0;
var source = Object.create(Iterator.prototype);
Object.defineProperty(source, "next", {
  get: function() {
    nextGets++;
    return function() {
      nextCalls++;
      return cursor < 3 ? { value: ++cursor, done: false } : { done: true };
    };
  }
});
source.return = function() { returnCalls++; return {}; };
var mapperCalls = 0;
var mapped = source.map(function(value, index) {
  mapperCalls++;
  return value * 10 + index;
});
var first = mapped.next();
var second = mapped.next();
var third = mapped.next();
var exhausted = mapped.next();
var after = mapped.next();
mapped.return();
var iterationOk = nextGets === 1 && nextCalls === 4 && mapperCalls === 3 &&
  first.value === 10 && first.done === false && second.value === 21 &&
  third.value === 32 && exhausted.done === true && after.done === true && returnCalls === 0;

var explicitReturnCalls = 0;
var closeSource = Object.create(Iterator.prototype);
closeSource.next = function() { return { value: 1, done: false }; };
closeSource.return = function() { explicitReturnCalls++; return {}; };
var closeHelper = closeSource.map(function(value) { return value; });
closeHelper.next();
var closeResult = closeHelper.return();
closeHelper.return();
var returnOk = explicitReturnCalls === 1 && closeResult.done === true;

var chainSource = Object.create(Iterator.prototype);
var chainCursor = 0;
chainSource.next = function() {
  return chainCursor++ === 0 ? { value: 5, done: false } : { done: true };
};
var chained = chainSource.map(function(value) { return value + 1; })
  .map(function(value, index) { return value + index + 1; });
var chainFirst = chained.next();
var chainDone = chained.next();
var chainOk = chainFirst.value === 7 && chainFirst.done === false && chainDone.done === true;

var reentryClosed = 0;
var reentrySource = Object.create(Iterator.prototype);
reentrySource.next = function() { return { value: 1, done: false }; };
reentrySource.return = function() { reentryClosed++; return {}; };
var reentryHelper;
reentryHelper = reentrySource.map(function(value) { reentryHelper.next(); return value; });
var reentryTypeError = false;
try { reentryHelper.next(); } catch (error) { reentryTypeError = error instanceof TypeError; }
var reentryDone = reentryHelper.next();
var reentryOk = reentryTypeError && reentryClosed === 1 && reentryDone.done === true;

invalidClosed === 2 && invalidTypeError && invalidObjectTypeError && iterationOk && returnOk &&
  chainOk && reentryOk;
"#;

const ITERATOR_FILTER_SOURCE: &str = r#"
var descriptor = Object.getOwnPropertyDescriptor(Iterator.prototype, "filter");
var shapeOk = typeof Iterator.prototype.filter === "function" &&
  Iterator.prototype.filter.name === "filter" && Iterator.prototype.filter.length === 1 &&
  descriptor.writable === true && descriptor.enumerable === false &&
  descriptor.configurable === true;

var invalidClosed = 0;
var invalidTypeError = false;
var invalid = {
  __proto__: Iterator.prototype,
  get next() { throw new Error("next must not be read"); },
  return() { invalidClosed++; return {}; }
};
try { invalid.filter(); } catch (error) { invalidTypeError = error instanceof TypeError; }

var values = [{ id: 1 }, { id: 2 }, { id: 3 }, { id: 4 }];
var cursor = 0;
var nextGets = 0;
var nextCalls = 0;
var source = Object.create(Iterator.prototype);
Object.defineProperty(source, "next", {
  get: function() {
    nextGets++;
    return function() {
      nextCalls++;
      return cursor < values.length ? { value: values[cursor++], done: false } : { done: true };
    };
  }
});
var seen = "";
var filtered = source.filter(function(value, index) {
  seen += value.id + ":" + index + ";";
  var pressure = { value: value, index: index };
  return pressure.index === 2 ? pressure : false;
});
var selected = filtered.next();
var exhausted = filtered.next();
var after = filtered.next();
var iterationOk = nextGets === 1 && nextCalls === 5 &&
  seen === "1:0;2:1;3:2;4:3;" && selected.value === values[2] &&
  selected.done === false && exhausted.done === true && after.done === true;

var returnCalls = 0;
var closeSource = Object.create(Iterator.prototype);
closeSource.next = function() { return { value: 1, done: false }; };
closeSource.return = function() { returnCalls++; return {}; };
var closeHelper = closeSource.filter(function() { return true; });
closeHelper.next();
var returned = closeHelper.return();
closeHelper.return();
var returnOk = returnCalls === 1 && returned.done === true;

var abruptReturnCalls = 0;
var original = new Error("predicate");
var abruptSource = Object.create(Iterator.prototype);
abruptSource.next = function() { return { value: 1, done: false }; };
abruptSource.return = function() { abruptReturnCalls++; throw new Error("close"); };
var abruptHelper = abruptSource.filter(function() { throw original; });
var preservedOriginal = false;
try { abruptHelper.next(); } catch (error) { preservedOriginal = error === original; }
var abruptDone = abruptHelper.next();
var abruptOk = preservedOriginal && abruptReturnCalls === 1 && abruptDone.done === true;

var protocolCloseCalls = 0;
var protocolSource = Object.create(Iterator.prototype);
protocolSource.next = function() { throw new Error("next"); };
protocolSource.return = function() { protocolCloseCalls++; return {}; };
var protocolHelper = protocolSource.filter(function() { return true; });
try { protocolHelper.next(); } catch (error) {}
var protocolOk = protocolCloseCalls === 0 && protocolHelper.next().done === true;

var chainCursor = 0;
var chainSource = Object.create(Iterator.prototype);
chainSource.next = function() {
  return chainCursor < 3 ? { value: ++chainCursor, done: false } : { done: true };
};
var chained = chainSource.map(function(value) { return value * 2; })
  .filter(function(value, index) { return value > 2 && index < 2; });
var chainFirst = chained.next();
var chainDone = chained.next();
var chainOk = chainFirst.value === 4 && chainFirst.done === false && chainDone.done === true;

var reentryClosed = 0;
var reentrySource = Object.create(Iterator.prototype);
reentrySource.next = function() { return { value: 1, done: false }; };
reentrySource.return = function() { reentryClosed++; return {}; };
var reentryHelper;
reentryHelper = reentrySource.filter(function() { reentryHelper.next(); return true; });
var reentryTypeError = false;
try { reentryHelper.next(); } catch (error) { reentryTypeError = error instanceof TypeError; }
var reentryDone = reentryHelper.next();
var reentryOk = reentryTypeError && reentryClosed === 1 && reentryDone.done === true;

shapeOk && invalidClosed === 1 && invalidTypeError && iterationOk && returnOk && abruptOk &&
  protocolOk && chainOk && reentryOk;
"#;

const ITERATOR_TAKE_DROP_SOURCE: &str = r#"
var takeDescriptor = Object.getOwnPropertyDescriptor(Iterator.prototype, "take");
var dropDescriptor = Object.getOwnPropertyDescriptor(Iterator.prototype, "drop");
var shapeOk = Iterator.prototype.take.name === "take" && Iterator.prototype.take.length === 1 &&
  takeDescriptor.writable === true && takeDescriptor.enumerable === false &&
  takeDescriptor.configurable === true && Iterator.prototype.drop.name === "drop" &&
  Iterator.prototype.drop.length === 1 && dropDescriptor.writable === true &&
  dropDescriptor.enumerable === false && dropDescriptor.configurable === true;

var order = "";
var values = [{ id: 1 }, { id: 2 }, { id: 3 }];
var cursor = 0;
var takeCloseCalls = 0;
var takeSource = Object.create(Iterator.prototype);
Object.defineProperty(takeSource, "next", {
  get: function() {
    order += "n";
    return function() {
      order += "c";
      return cursor < values.length ? { value: values[cursor++], done: false } : { done: true };
    };
  }
});
takeSource.return = function() { takeCloseCalls++; return {}; };
var limit = { valueOf: function() { order += "v"; return 2.9; } };
var taken = takeSource.take(limit);
var takeFirst = taken.next();
var takeSecond = taken.next();
var takeDone = taken.next();
var takeAfter = taken.next();
var takeOk = order === "vncc" && takeFirst.value === values[0] &&
  takeSecond.value === values[1] && takeDone.done === true && takeAfter.done === true &&
  takeCloseCalls === 1;

var zeroNextCalls = 0;
var zeroCloseCalls = 0;
var zeroSource = Object.create(Iterator.prototype);
zeroSource.next = function() { zeroNextCalls++; return { value: 1, done: false }; };
zeroSource.return = function() { zeroCloseCalls++; return {}; };
var zeroDone = zeroSource.take(-0.5).next();
var zeroOk = zeroDone.done === true && zeroNextCalls === 0 && zeroCloseCalls === 1;

var invalidCloseCalls = 0;
var invalidSource = Object.create(Iterator.prototype);
Object.defineProperty(invalidSource, "next", {
  get: function() { throw new Error("next must not be read"); }
});
invalidSource.return = function() { invalidCloseCalls++; return {}; };
var invalidRange = false;
try { invalidSource.drop(NaN); } catch (error) { invalidRange = error instanceof RangeError; }

var droppedValueGets = 0;
var dropCursor = 0;
var dropSource = Object.create(Iterator.prototype);
dropSource.next = function() {
  var current = ++dropCursor;
  if (current > 4) return { done: true };
  var result = { done: false };
  Object.defineProperty(result, "value", {
    get: function() { droppedValueGets++; return { id: current }; }
  });
  return result;
};
var dropped = dropSource.drop({ valueOf: function() { return 2; } });
var dropFirst = dropped.next();
var dropSecond = dropped.next();
var dropDone = dropped.next();
var dropOk = dropFirst.value.id === 3 && dropSecond.value.id === 4 && dropDone.done === true &&
  droppedValueGets === 2;

var infinityCalls = 0;
var infinitySource = Object.create(Iterator.prototype);
infinitySource.next = function() {
  infinityCalls++;
  return infinityCalls < 4 ? { value: infinityCalls, done: false } : { done: true };
};
var infinityDone = infinitySource.drop(Infinity).next();
var infinityOk = infinityDone.done === true && infinityCalls === 4;

var reentrySource = Object.create(Iterator.prototype);
var reentryHelper;
reentrySource.next = function() { reentryHelper.next(); return { done: true }; };
reentryHelper = reentrySource.take(1);
var reentryTypeError = false;
try { reentryHelper.next(); } catch (error) { reentryTypeError = error instanceof TypeError; }
var reentryOk = reentryTypeError && reentryHelper.next().done === true;

shapeOk && takeOk && zeroOk && invalidRange && invalidCloseCalls === 1 && dropOk &&
  infinityOk && reentryOk;
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

#[test]
fn iterator_map_is_stable_for_every_dispatch_batch() {
    assert_iterator_map::<1>(8_971, false);
    assert_iterator_map::<2>(8_972, false);
    assert_iterator_map::<4>(8_974, false);
    assert_iterator_map::<8>(8_978, false);
    assert_iterator_map::<16>(8_986, false);
}

#[test]
fn iterator_map_roots_survive_forced_major_collection() {
    assert_iterator_map::<8>(8_990, true);
}

#[test]
fn iterator_filter_is_stable_for_every_dispatch_batch() {
    assert_iterator_filter::<1>(9_001, false);
    assert_iterator_filter::<2>(9_002, false);
    assert_iterator_filter::<4>(9_004, false);
    assert_iterator_filter::<8>(9_008, false);
    assert_iterator_filter::<16>(9_016, false);
}

#[test]
fn iterator_filter_roots_survive_forced_major_collection() {
    assert_iterator_filter::<8>(9_020, true);
}

#[test]
fn iterator_take_and_drop_are_stable_for_every_dispatch_batch() {
    assert_iterator_take_drop::<1>(9_031, false);
    assert_iterator_take_drop::<2>(9_032, false);
    assert_iterator_take_drop::<4>(9_034, false);
    assert_iterator_take_drop::<8>(9_038, false);
    assert_iterator_take_drop::<16>(9_046, false);
}

#[test]
fn iterator_take_and_drop_roots_survive_forced_major_collection() {
    assert_iterator_take_drop::<8>(9_050, true);
}

#[test]
fn lazy_helpers_do_not_grow_the_rust_stack_for_large_native_loops() {
    let source = r#"
var values = "x".repeat(2000);
var dropped = values[Symbol.iterator]().drop(2000).next();
var filtered = values[Symbol.iterator]().filter(Number.isNaN).next();
dropped.done === true && dropped.value === undefined &&
  filtered.done === true && filtered.value === undefined;
"#;
    let module = compile_iterator_source(source, 9_051);
    let mut isolate = test_isolate_with_heap_spans(128);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_194_304,
                quantum: 4_194_304,
            },
        )
        .expect("large native lazy-helper fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "large native lazy helper returned {outcome:?}"
    );
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

/// Executes map creation, stepping, exhaustion, and explicit close under one VM policy.
fn assert_iterator_map<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_iterator_source(ITERATOR_MAP_SOURCE, source_id);
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
        .expect("Iterator.map fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(error) => isolate.native_error_kind(error).ok().flatten(),
        RunOutcome::Completed(_) | RunOutcome::BudgetExhausted => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Executes filter's rejection loop, retained value, close policy, and re-entry behavior.
fn assert_iterator_filter<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_iterator_source(ITERATOR_FILTER_SOURCE, source_id);
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
        .expect("Iterator.filter fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(error) => isolate.native_error_kind(error).ok().flatten(),
        RunOutcome::Completed(_) | RunOutcome::BudgetExhausted => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Executes take/drop conversion, skipping, early close, re-entry, and exhaustion behavior.
fn assert_iterator_take_drop<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_iterator_source(ITERATOR_TAKE_DROP_SOURCE, source_id);
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
        .expect("Iterator.take/drop fixture executes");
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
