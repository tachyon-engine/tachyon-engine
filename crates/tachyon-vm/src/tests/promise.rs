use std::sync::Arc;
use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{ForcedCollectionMode, SPAN_SIZE_BYTES};

use super::super::*;

fn test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024).with_max_shapes(2_048),
    ))
    .unwrap()
}

#[test]
fn promise_jobs_move_through_the_traced_active_slot_in_fifo_order() {
    let mut queue = PromiseJobQueue::new();
    queue.push(PromiseJob::Reaction {
        handler: Value::from_i32(1),
        capability: Value::from_i32(2),
        argument: Value::from_i32(3),
        rejected: false,
    });
    queue.push(PromiseJob::Thenable {
        promise: Value::from_i32(4),
        thenable: Value::from_i32(5),
        then: Value::from_i32(6),
    });
    assert_eq!(queue.len(), 2);
    assert!(matches!(
        queue.begin_next(),
        Some(PromiseJob::Reaction { argument, .. }) if argument.as_i32() == Some(3)
    ));
    assert_eq!(queue.len(), 1);
    queue.finish_active();
    assert!(matches!(
        queue.begin_next(),
        Some(PromiseJob::Thenable { then, .. }) if then.as_i32() == Some(6)
    ));
}

#[test]
fn resolving_functions_share_the_first_call_guard_across_forced_major() {
    let mut isolate = test_isolate();
    let promise = isolate
        .create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )
        .unwrap();
    let arguments = isolate
        .create_promise_capability_arguments(promise)
        .unwrap();
    let arguments = isolate.native_call_state_snapshot(arguments).unwrap();
    let resolve = arguments.values[0];
    let reject = arguments.values[1];
    isolate.fiber.registers = vec![promise, resolve, reject];
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let FunctionExecutable::PromiseResolver {
        cell,
        reject: false,
    } = isolate.resolve_function_object(resolve).unwrap().executable
    else {
        panic!("resolve capability must use the shared cell")
    };
    let claimed = isolate.claim_promise_resolver(cell).unwrap().unwrap();
    isolate
        .settle_promise(claimed, PromiseState::Fulfilled, Value::from_i32(7))
        .unwrap();
    let FunctionExecutable::PromiseResolver { cell, reject: true } =
        isolate.resolve_function_object(reject).unwrap().executable
    else {
        panic!("reject capability must use the shared cell")
    };
    assert!(isolate.claim_promise_resolver(cell).unwrap().is_none());
    let snapshot = isolate.promise_snapshot(promise).unwrap();
    assert_eq!(snapshot.state, PromiseState::Fulfilled);
    assert_eq!(snapshot.result.as_i32(), Some(7));
}

#[test]
fn promise_species_accessor_descriptor_round_trips() {
    let mut isolate = test_isolate();
    let constructor = isolate.realm.promise_constructor.unwrap();
    let species = isolate.realm.well_known_symbols.species.unwrap();
    let key = isolate.property_key(species).unwrap();
    let descriptor = isolate
        .complete_own_property_descriptor(constructor, key)
        .unwrap()
        .unwrap();
    let PropertyDescriptor::Accessor(accessor) = descriptor else {
        panic!("Promise @@species must remain an accessor")
    };
    assert!(accessor.getter.is_some_and(|getter| {
        matches!(
            isolate.resolve_function_object(getter).unwrap().executable,
            FunctionExecutable::Native(NativeFunction::SpeciesGetter)
        )
    }));
    assert_eq!(
        accessor.setter.and_then(Value::as_immediate),
        Some(Immediate::Undefined)
    );
    assert_eq!(accessor.enumerable, Some(false));
    assert_eq!(accessor.configurable, Some(true));

    let result = isolate.create_ordinary_object().unwrap();
    isolate
        .materialize_property_descriptor(result, descriptor)
        .unwrap();
}

#[test]
fn promise_to_string_tag_descriptor_is_standard() {
    let mut isolate = test_isolate();
    let prototype = isolate.realm.promise_prototype.unwrap();
    let symbol = isolate.realm.well_known_symbols.to_string_tag.unwrap();
    let key = isolate.property_key(symbol).unwrap();
    let descriptor = isolate
        .complete_own_property_descriptor(prototype, key)
        .unwrap()
        .unwrap();
    let PropertyDescriptor::Data(data) = descriptor else {
        panic!("Promise @@toStringTag must remain a data property")
    };
    assert_eq!(
        isolate.string_value_to_utf16(data.value.unwrap()).unwrap(),
        "Promise".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(data.writable, Some(false));
    assert_eq!(data.enumerable, Some(false));
    assert_eq!(data.configurable, Some(true));
}

const PROMISE_ALL_SETUP: &str = r#"
var allTrace = 0;
var allReject = 0;
var emptyLength = -1;
var resolveLater;
var later = new Promise(function(resolve) { resolveLater = resolve; });
Promise.all([1, later, 3]).then(function(values) {
  allTrace = values[0] * 100 + values[1] * 10 + values[2];
}, function() { allTrace = -1; });
Promise.all([Promise.resolve(1), Promise.reject(9)]).then(
  function() { allReject = -1; },
  function(reason) { allReject = reason; }
);
Promise.all([]).then(function(values) { emptyLength = values.length; });
resolveLater(2);
"#;

const PROMISE_ALL_ITERABLE_SETUP: &str = r#"
var iterableTrace = 0;
var resolveCalls = 0;
var originalResolve = Promise.resolve;
Promise.resolve = function(value) {
  resolveCalls = resolveCalls + 1;
  return originalResolve.call(Promise, value);
};
var iterable = {};
iterable[Symbol.iterator] = function() {
  var index = 0;
  return {
    next: function() {
      index = index + 1;
      if (index === 1) return { value: 4, done: false };
      if (index === 2) return { value: Promise.resolve(5), done: false };
      return { value: undefined, done: true };
    }
  };
};
Promise.all(iterable).then(function(values) {
  iterableTrace = values[0] * 10 + values[1];
});
"#;

const PROMISE_ALL_ABRUPT_SETUP: &str = r#"
var abruptOutside = 0;
var abruptReason = 0;
var closeCount = 0;
var abruptIterable = {};
abruptIterable[Symbol.iterator] = function() {
  var index = 0;
  return {
    next: function() {
      index = index + 1;
      if (index === 1) return { value: 1, done: false };
      throw 9;
    },
    return: function() {
      closeCount = closeCount + 1;
      throw 10;
    }
  };
};
try {
  Promise.all(abruptIterable).then(
    function() { abruptReason = -1; },
    function(reason) { abruptReason = reason; }
  );
} catch (error) {
  abruptOutside = error;
}
"#;

const PROMISE_ALL_RESOLVE_GETTER_THROW_SETUP: &str = r#"
var resolveGetterOutside = 0;
var resolveGetterReason = 0;
Object.defineProperty(Promise, "resolve", {
  configurable: true,
  get: function() { throw 17; }
});
try {
  Promise.all([]).then(
    function() { resolveGetterReason = -1; },
    function(reason) { resolveGetterReason = reason; }
  );
} catch (error) {
  resolveGetterOutside = error;
}
"#;

const PROMISE_ALL_THEN_GETTER_CLOSE_SETUP: &str = r#"
var thenGetterReason = 0;
var thenGetterCalls = 0;
var closeGetterCalls = 0;
var closeCallCount = 0;
var originalResolveForClose = Promise.resolve;
Promise.resolve = function(value) {
  var result = {};
  Object.defineProperty(result, "then", {
    get: function() {
      thenGetterCalls = thenGetterCalls + 1;
      throw 21;
    }
  });
  return result;
};
var closeIterable = {};
closeIterable[Symbol.iterator] = function() {
  var done = false;
  var iterator = {
    next: function() {
      if (done) return { value: undefined, done: true };
      done = true;
      return { value: 1, done: false };
    }
  };
  Object.defineProperty(iterator, "return", {
    get: function() {
      closeGetterCalls = closeGetterCalls + 1;
      throw 22;
    }
  });
  return iterator;
};
Promise.all(closeIterable).then(undefined, function(reason) {
  thenGetterReason = reason;
});
Promise.resolve = originalResolveForClose;
"#;

const PROMISE_ALL_CUSTOM_CAPABILITY_SETUP: &str = r#"
var capabilityResult = 0;
var capabilityReject = 0;
var capabilityResolveCalls = 0;
function CustomPromise(executor) {
  executor(function(values) {
    capabilityResult = values[0] * 10 + values[1];
  }, function(reason) {
    capabilityReject = reason;
  });
}
CustomPromise.resolve = function(value) {
  capabilityResolveCalls = capabilityResolveCalls + 1;
  return {
    then: function(onFulfilled, onRejected) {
      onFulfilled(value);
    }
  };
};
var customCapabilityResult = Promise.all.call(CustomPromise, [2, 3]);
"#;

const PROMISE_RACE_SETUP: &str = r#"
var raceFulfill = 0;
var raceReject = 0;
var emptySettled = 0;
var customRaceValue = 0;
var customRaceReject = 0;
Promise.race([7, 8]).then(function(value) { raceFulfill = value; });
Promise.race([Promise.reject(9), 10]).then(
  function() { raceReject = -1; },
  function(reason) { raceReject = reason; }
);
Promise.race([]).then(function() { emptySettled = 1; }, function() { emptySettled = -1; });
function RaceCapability(executor) {
  executor(function(value) { customRaceValue = value; }, function(reason) {
    customRaceReject = reason;
  });
}
RaceCapability.resolve = function(value) {
  return { then: function(onFulfilled, onRejected) { onFulfilled(value); } };
};
var customRace = Promise.race.call(RaceCapability, [11]);
"#;

const PROMISE_ALL_SETTLED_SETUP: &str = r#"
var settledFulfilledStatus = "";
var settledFulfilledValue = 0;
var settledRejectedStatus = "";
var settledRejectedReason = 0;
var settledEmptyLength = -1;
var customSettledTrace = "";
var customSettledReject = 0;
Promise.allSettled([3, Promise.reject(7)]).then(function(values) {
  settledFulfilledStatus = values[0].status;
  settledFulfilledValue = values[0].value;
  settledRejectedStatus = values[1].status;
  settledRejectedReason = values[1].reason;
});
Promise.allSettled([]).then(function(values) { settledEmptyLength = values.length; });
function SettledCapability(executor) {
  executor(function(values) {
    customSettledTrace = values[0].status + values[0].value + values[1].status + values[1].reason;
  }, function(reason) { customSettledReject = reason; });
}
SettledCapability.resolve = function(value) {
  return {
    then: function(onFulfilled, onRejected) {
      if (value === 4) onFulfilled(value);
      else onRejected(value);
    }
  };
};
var customSettled = Promise.allSettled.call(SettledCapability, [4, 5]);
"#;

const PROMISE_ANY_SETUP: &str = r#"
var anyValue = 0;
var anyErrorBrand = false;
var anyErrorsTrace = 0;
var emptyAnyLength = -1;
var aggregateMessage = "";
var aggregateCause = 0;
var aggregateErrorsEnumerable = true;
Promise.any([Promise.reject(1), 2]).then(function(value) { anyValue = value; });
Promise.any([Promise.reject(3), Promise.reject(4)]).then(
  function() { anyErrorsTrace = -1; },
  function(error) {
    anyErrorBrand = error instanceof AggregateError;
    anyErrorsTrace = error.errors[0] * 10 + error.errors[1];
  }
);
Promise.any([]).then(
  function() { emptyAnyLength = -2; },
  function(error) { emptyAnyLength = error.errors.length; }
);
var aggregate = new AggregateError([5, 6], "many", { cause: 7 });
aggregateMessage = aggregate.message;
aggregateCause = aggregate.cause;
aggregateErrorsEnumerable = Object.getOwnPropertyDescriptor(aggregate, "errors").enumerable;
"#;

const PROMISE_TRY_SETUP: &str = r#"
var tryValue = 0;
var tryReason = 0;
var tryArguments = 0;
var tryArgumentIdentity = false;
var tryReturnIdentity = false;
var tryThrowIdentity = false;
var nestedTryReturnIdentity = false;
var nestedTryArguments = false;
var nestedTryThrowIdentity = false;
var asyncStyleTryStatus = 0;
var customTryValue = 0;
var customTryReason = 0;
var customConstructorCalls = 0;
Promise.try(function(a, b, c) {
  tryArguments = a * 100 + b * 10 + c;
  return { then: function(resolve) { resolve(7); } };
}, 1, 2, 3).then(function(value) { tryValue = value; });
Promise.try(function() { throw 9; }).then(
  function() { tryReason = -1; },
  function(reason) { tryReason = reason; }
);
var trySentinel = { sentinel: true };
Promise.try(function() { return trySentinel; }).then(function(value) {
  tryReturnIdentity = value === trySentinel;
});
Promise.try(function() {
  tryArgumentIdentity = arguments.length === 3 && arguments[0] === 1 &&
    arguments[1] === trySentinel && arguments[2] === 3;
}, 1, trySentinel, 3);
Promise.try(function() { throw trySentinel; }).then(undefined, function(reason) {
  tryThrowIdentity = reason === trySentinel;
});
function runNestedTryReturn() {
  return Promise.try(function() { return trySentinel; }).then(function(value) {
    nestedTryReturnIdentity = value === trySentinel;
  });
}
function runNestedTryArguments() {
  return Promise.try(function() {
    nestedTryArguments = arguments.length === 3 && arguments[0] === 1 &&
      arguments[1] === trySentinel && arguments[2] === 3;
  }, 1, trySentinel, 3);
}
function runNestedTryThrow() {
  return Promise.try(function() { throw trySentinel; }).then(undefined, function(reason) {
    nestedTryThrowIdentity = reason === trySentinel;
  });
}
runNestedTryReturn();
runNestedTryArguments();
runNestedTryThrow();
function invokeAsyncStyle(testFunction) {
  try {
    testFunction().then(function() {
      asyncStyleTryStatus = 1;
    }, function() {
      asyncStyleTryStatus = 2;
    });
  } catch (error) {
    asyncStyleTryStatus = 3;
  }
}
invokeAsyncStyle(function() {
  return Promise.try(function() { return trySentinel; }).then(function(value) {
    if (value !== trySentinel) throw 17;
  });
});
function TryCapability(executor) {
  customConstructorCalls = customConstructorCalls + 1;
  executor(function(value) { customTryValue = value; }, function(reason) {
    customTryReason = reason;
  });
}
var customTry = Promise.try.call(TryCapability, function(a, b) { return a + b; }, 4, 5);
function RejectCapability(executor) {
  executor(function() { customTryReason = -1; }, function(reason) {
    customTryReason = reason;
  });
}
Promise.try.call(RejectCapability, function() { throw 11; });
"#;

const PROMISE_CATCH_INVOKE_SOURCE: &str = r#"
var target = {};
var returned = {};
var calls = 0;
var receiver;
var count;
var first;
var second;
target.then = function(a, b) {
  calls++;
  receiver = this;
  count = arguments.length;
  first = a;
  second = b;
  return returned;
};
var result = Promise.prototype.catch.call(target, 1, 2, 3);
var score = 0;
if (calls === 1) score += 1;
if (receiver === target) score += 2;
if (count === 2) score += 4;
if (first === undefined) score += 8;
if (second === 1) score += 16;
if (result === returned) score += 32;
score;
"#;

const PROMISE_CONSTRUCTOR_PROTOTYPE_SOURCE: &str = r#"
var accessorTrace = "";
var accessorPrototype = {};
var accessorTarget = (function() {}).bind(undefined);
Object.defineProperty(accessorTarget, "prototype", {
  get: function() {
    accessorTrace = accessorTrace + "g";
    return accessorPrototype;
  }
});
var accessorPromise = Reflect.construct(Promise, [function(resolve) {
  accessorTrace = accessorTrace + "e";
  resolve(1);
}], accessorTarget);

var proxyTrace = "";
var proxyReceiver = false;
var proxyPrototype = {};
function ProxyTarget() {}
var proxyTarget;
proxyTarget = new Proxy(ProxyTarget, {
  get: function(target, key, receiver) {
    proxyTrace = proxyTrace + key;
    proxyReceiver = receiver === proxyTarget;
    return proxyPrototype;
  }
});
var proxyPromise = Reflect.construct(Promise, [function(resolve) {
  proxyTrace = proxyTrace + ":executor";
  resolve(2);
}], proxyTarget);

var abruptSentinel = {};
var abruptExecutorCalls = 0;
var abruptIdentity = false;
var abruptTarget = (function() {}).bind(undefined);
Object.defineProperty(abruptTarget, "prototype", {
  get: function() { throw abruptSentinel; }
});
try {
  Reflect.construct(Promise, [function() { abruptExecutorCalls++; }], abruptTarget);
} catch (error) {
  abruptIdentity = error === abruptSentinel;
}

accessorTrace === "ge" && Object.getPrototypeOf(accessorPromise) === accessorPrototype &&
  proxyTrace === "prototype:executor" && proxyReceiver &&
  Object.getPrototypeOf(proxyPromise) === proxyPrototype &&
  abruptIdentity && abruptExecutorCalls === 0;
"#;

const PROMISE_CONSTRUCTOR_CROSS_REALM_SOURCE: &str = r#"
var crossRealmPromise = Reflect.construct(Promise, [function(resolve) {
  resolve(3);
}], foreignPromiseConstructor);
globalThis.crossRealmPromiseResult = crossRealmPromise;
crossRealmPromise !== undefined;
"#;

#[test]
fn promise_all_intrinsic_path_is_stable_for_every_dispatch_batch() {
    assert_promise_all_source::<1>(5_101);
    assert_promise_all_source::<2>(5_102);
    assert_promise_all_source::<4>(5_104);
    assert_promise_all_source::<8>(5_108);
    assert_promise_all_source::<16>(5_116);
}

#[test]
fn promise_all_handlers_survive_forced_major_collection() {
    let setup = compile_promise_source(5_120, PROMISE_ALL_SETUP);
    let probe = compile_promise_source(
        5_121,
        "allTrace === 123 && allReject === 9 && emptyLength === 0;",
    );
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    run_promise_module::<8>(&mut isolate, &setup);
    let outcome = run_promise_module::<8>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

#[test]
fn promise_all_generic_iterable_is_stable_for_every_dispatch_batch() {
    assert_promise_all_iterable_source::<1>(5_201, false);
    assert_promise_all_iterable_source::<2>(5_203, false);
    assert_promise_all_iterable_source::<4>(5_205, false);
    assert_promise_all_iterable_source::<8>(5_207, false);
    assert_promise_all_iterable_source::<16>(5_209, false);
    assert_promise_all_iterable_source::<8>(5_211, true);
}

#[test]
fn promise_all_closes_iterator_and_rejects_with_original_throw() {
    assert_promise_all_abrupt_source::<1>(5_301, false);
    assert_promise_all_abrupt_source::<8>(5_303, false);
    assert_promise_all_abrupt_source::<16>(5_305, false);
    assert_promise_all_abrupt_source::<8>(5_307, true);
}

#[test]
fn promise_all_rejects_when_get_promise_resolve_throws() {
    let setup = compile_promise_source(5_401, PROMISE_ALL_RESOLVE_GETTER_THROW_SETUP);
    let probe = compile_promise_source(
        5_402,
        "resolveGetterOutside === 0 && resolveGetterReason === 17;",
    );
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    run_promise_module::<8>(&mut isolate, &setup);
    let outcome = run_promise_module::<8>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

#[test]
fn promise_all_then_getter_abrupt_closes_and_preserves_original_reason() {
    assert_promise_all_then_getter_close::<1>(5_421, false);
    assert_promise_all_then_getter_close::<8>(5_423, false);
    assert_promise_all_then_getter_close::<16>(5_425, false);
    assert_promise_all_then_getter_close::<8>(5_427, true);
}

#[test]
fn promise_all_custom_capability_is_stable_for_every_dispatch_batch() {
    assert_promise_all_custom_capability::<1>(5_501, false);
    assert_promise_all_custom_capability::<2>(5_503, false);
    assert_promise_all_custom_capability::<4>(5_505, false);
    assert_promise_all_custom_capability::<8>(5_507, false);
    assert_promise_all_custom_capability::<16>(5_509, false);
    assert_promise_all_custom_capability::<8>(5_511, true);
}

#[test]
fn promise_race_is_stable_for_every_dispatch_batch_and_forced_major() {
    assert_promise_race::<1>(5_601, false);
    assert_promise_race::<2>(5_603, false);
    assert_promise_race::<4>(5_605, false);
    assert_promise_race::<8>(5_607, false);
    assert_promise_race::<16>(5_609, false);
    assert_promise_race::<8>(5_611, true);
}

#[test]
fn promise_all_settled_is_stable_for_every_dispatch_batch_and_forced_major() {
    assert_promise_all_settled::<1>(5_701, false);
    assert_promise_all_settled::<2>(5_703, false);
    assert_promise_all_settled::<4>(5_705, false);
    assert_promise_all_settled::<8>(5_707, false);
    assert_promise_all_settled::<16>(5_709, false);
    assert_promise_all_settled::<8>(5_711, true);
}

#[test]
fn promise_any_and_aggregate_error_are_stable_for_every_batch_and_forced_major() {
    assert_promise_any::<1>(5_801, false);
    assert_promise_any::<2>(5_803, false);
    assert_promise_any::<4>(5_805, false);
    assert_promise_any::<8>(5_807, false);
    assert_promise_any::<16>(5_809, false);
    assert_promise_any::<8>(5_811, true);
}

#[test]
fn promise_try_is_stable_for_every_dispatch_batch_and_forced_major() {
    assert_promise_try::<1>(5_901, false);
    assert_promise_try::<2>(5_903, false);
    assert_promise_try::<4>(5_905, false);
    assert_promise_try::<8>(5_907, false);
    assert_promise_try::<16>(5_909, false);
    assert_promise_try::<8>(5_911, true);
}

#[test]
fn promise_catch_invokes_generic_then_with_exact_arguments() {
    let module = compile_promise_source(5_950, PROMISE_CATCH_INVOKE_SOURCE);
    let mut isolate = test_isolate();
    let outcome = run_promise_module::<8>(&mut isolate, &module);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(63)),
        "generic Promise.catch returned {outcome:?}, score={:?}",
        match outcome {
            RunOutcome::Completed(value) => value.as_i32(),
            _ => None,
        }
    );
}

#[test]
fn promise_constructor_prototype_get_is_resumable_for_every_batch_and_forced_major() {
    assert_promise_constructor_prototype::<1>(5_961, false);
    assert_promise_constructor_prototype::<2>(5_962, false);
    assert_promise_constructor_prototype::<4>(5_964, false);
    assert_promise_constructor_prototype::<8>(5_968, false);
    assert_promise_constructor_prototype::<16>(5_976, false);
    assert_promise_constructor_prototype::<8>(5_984, true);
}

/// Verifies non-object fallback selects `%Promise.prototype%` from newTarget's defining Realm.
#[test]
fn promise_constructor_non_object_prototype_uses_new_target_realm_fallback() {
    let module = compile_promise_source(5_990, PROMISE_CONSTRUCTOR_CROSS_REALM_SOURCE);
    let child_module = compile_promise_source(
        5_991,
        "var ForeignPromiseTarget = function() {}; ForeignPromiseTarget.prototype = 1;",
    );
    let mut isolate = test_isolate();
    let (child_realm, _) = isolate.create_realm().expect("child Realm initializes");
    let child_outcome = isolate
        .execute_in_realm(
            child_realm,
            &child_module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("child Realm constructor fixture executes");
    assert!(matches!(child_outcome, RunOutcome::Completed(_)));
    let constructor_atom = isolate
        .intern_intrinsic_name(b"ForeignPromiseTarget")
        .unwrap();
    let foreign_constructor = isolate
        .inactive_realms
        .iter()
        .find(|(realm, _)| *realm == child_realm)
        .and_then(|(_, realm)| {
            realm
                .resolve(constructor_atom)
                .and_then(|slot| realm.get_slot(slot))
        })
        .expect("child Realm publishes constructor");
    let foreign_prototype = isolate
        .realm_intrinsic_prototype(child_realm, IntrinsicPrototypeKind::Promise)
        .expect("child Realm publishes Promise prototype");
    let foreign_atom = isolate
        .intern_intrinsic_name(b"foreignPromiseConstructor")
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
    let outcome = run_promise_module::<8>(&mut isolate, &module);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "cross-Realm Promise construction returned {outcome:?}"
    );
    let result_atom = isolate
        .intern_intrinsic_name(b"crossRealmPromiseResult")
        .unwrap();
    let result = isolate
        .get_data_property(global, result_atom)
        .unwrap()
        .expect("fixture publishes cross-Realm Promise");
    assert_eq!(
        isolate.object_prototype_of(result).unwrap(),
        foreign_prototype,
        "non-object prototype must fall back to newTarget Realm's Promise prototype"
    );
}

/// Executes the setup and probe under one interpreter dispatch batch.
fn assert_promise_all_source<const N: usize>(source_id: u32) {
    let setup = compile_promise_source(source_id, PROMISE_ALL_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "allTrace === 123 && allReject === 9 && emptyLength === 0;",
    );
    let mut isolate = test_isolate();
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Exercises every observable generic-iterator stage and optionally forces GC at allocations.
fn assert_promise_all_iterable_source<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_ALL_ITERABLE_SETUP);
    let probe =
        compile_promise_source(source_id + 1, "iterableTrace === 45 && resolveCalls === 3;");
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Verifies an abrupt IteratorStep rejects without closing its already-done iterator record.
fn assert_promise_all_abrupt_source<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_ALL_ABRUPT_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "abruptOutside === 0 && abruptReason === 9 && closeCount === 0;",
    );
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Verifies `then` abrupt closes the iterator while close failure preserves the first throw.
fn assert_promise_all_then_getter_close<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_ALL_THEN_GETTER_CLOSE_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "thenGetterReason === 21 && thenGetterCalls === 1 && closeGetterCalls === 1 && closeCallCount === 0;",
    );
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Verifies generic NewPromiseCapability callbacks and constructor identity across GC.
fn assert_promise_all_custom_capability<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_ALL_CUSTOM_CAPABILITY_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "customCapabilityResult instanceof CustomPromise && capabilityResult === 23 && capabilityReject === 0 && capabilityResolveCalls === 2;",
    );
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Covers both intrinsic settlement directions, empty iteration, and a custom capability.
fn assert_promise_race<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_RACE_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "raceFulfill === 7 && raceReject === 9 && emptySettled === 0 && customRaceValue === 11 && customRaceReject === 0 && customRace instanceof RaceCapability;",
    );
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Covers both settled record shapes, empty input, and generic capability settlement.
fn assert_promise_all_settled<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_ALL_SETTLED_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "settledFulfilledStatus === 'fulfilled' && settledFulfilledValue === 3 && settledRejectedStatus === 'rejected' && settledRejectedReason === 7 && settledEmptyLength === 0 && customSettledTrace === 'fulfilled4rejected5' && customSettledReject === 0 && customSettled instanceof SettledCapability;",
    );
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Covers fulfillment, ordered rejection aggregation, empty input, and public construction.
fn assert_promise_any<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_ANY_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "anyValue === 2 && anyErrorBrand && anyErrorsTrace === 34 && emptyAnyLength === 0 && aggregateMessage === 'many' && aggregateCause === 7 && aggregateErrorsEnumerable === false;",
    );
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Covers intrinsic assimilation/rejection and generic capability calls with exact arguments.
fn assert_promise_try<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_TRY_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "tryValue === 7 && tryReason === 9 && tryArguments === 123 && tryArgumentIdentity && tryReturnIdentity && tryThrowIdentity && nestedTryReturnIdentity && nestedTryArguments && nestedTryThrowIdentity && asyncStyleTryStatus === 1 && customTryValue === 9 && customTryReason === 11 && customConstructorCalls === 1 && customTry instanceof TryCapability;",
    );
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    run_promise_module::<N>(&mut isolate, &setup);
    let outcome = run_promise_module::<N>(&mut isolate, &probe);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Covers accessor and Proxy prototype Get ordering, receiver identity, abruptness, and roots.
fn assert_promise_constructor_prototype<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_promise_source(source_id, PROMISE_CONSTRUCTOR_PROTOTYPE_SOURCE);
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = run_promise_module::<N>(&mut isolate, &module);
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles one Promise fixture while retaining no Oxc arena state.
fn compile_promise_source(source_id: u32, source: &str) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("promise-all-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Promise.all fixture compiles")
}

/// Runs one Promise fixture with enough fuel to drain all queued reactions.
fn run_promise_module<const N: usize>(
    isolate: &mut Isolate,
    module: &CompiledModule,
) -> RunOutcome {
    isolate
        .execute_with_batch::<N>(
            module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .unwrap_or_else(|error| panic!("Promise fixture batch {N} executes: {error:?}"))
}
