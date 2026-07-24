use std::sync::Arc;
use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{ForcedCollectionMode, SPAN_SIZE_BYTES};

use super::super::*;

fn test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024).with_max_shapes(384),
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

/// Verifies abrupt iteration rejects instead of escaping and closes with original-error priority.
fn assert_promise_all_abrupt_source<const N: usize>(source_id: u32, forced_major: bool) {
    let setup = compile_promise_source(source_id, PROMISE_ALL_ABRUPT_SETUP);
    let probe = compile_promise_source(
        source_id + 1,
        "abruptOutside === 0 && abruptReason === 9 && closeCount === 1;",
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
        .expect("Promise.all fixture executes")
}
