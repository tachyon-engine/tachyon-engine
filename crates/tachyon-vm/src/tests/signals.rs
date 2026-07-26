use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const SIGNAL_SOURCE: &str = r#"
var state = new Signal.State(2);
var calls = 0;
var doubled = new Signal.Computed(function() {
    calls = calls + 1;
    return state.get() * 2;
});
var notifications = 0;
var watcher = new Signal.subtle.Watcher(function() { notifications = notifications + 1; });
watcher.watch(doubled);
var cold = doubled.get() === 4 && calls === 1;
var warm = doubled.get() === 4 && calls === 1;
state.set(3);
var pending = watcher.getPending();
var invalidated = pending.length === 1 && pending[0] === doubled;
var recomputed = doubled.get() === 6 && calls === 2;
watcher.watch();
var rearmed = watcher.getPending().length === 0;
watcher.unwatch(doubled);
state.set(4);
var detached = watcher.getPending().length === 0;
cold && warm && invalidated && recomputed && rearmed && detached &&
Signal.State.name === "State" && Signal.Computed.name === "Computed" &&
Signal.subtle.Watcher.name === "Watcher";
"#;

const SIGNAL_API_CONTRACT_SOURCE: &str = r#"
function builtinDescriptor(object, key, writable, configurable) {
    var descriptor = Object.getOwnPropertyDescriptor(object, key);
    return descriptor !== undefined && descriptor.writable === writable &&
        descriptor.enumerable === false && descriptor.configurable === configurable;
}
var globalDescriptor = builtinDescriptor(this, "Signal", true, true);
var namespaceDescriptors = builtinDescriptor(Signal, "State", true, true) &&
    builtinDescriptor(Signal, "Computed", true, true) &&
    builtinDescriptor(Signal, "subtle", true, true) &&
    builtinDescriptor(Signal.subtle, "Watcher", true, true) &&
    builtinDescriptor(Signal.subtle, "watched", true, true) &&
    builtinDescriptor(Signal.subtle, "unwatched", true, true) &&
    typeof Signal.subtle.watched === "symbol" && typeof Signal.subtle.unwatched === "symbol" &&
    Signal.subtle.watched !== Signal.subtle.unwatched;
var prototypeDescriptors = builtinDescriptor(Signal.State.prototype, "constructor", true, true) &&
    builtinDescriptor(Signal.State.prototype, "get", true, true) &&
    builtinDescriptor(Signal.State.prototype, "set", true, true) &&
    builtinDescriptor(Signal.Computed.prototype, "constructor", true, true) &&
    builtinDescriptor(Signal.Computed.prototype, "get", true, true) &&
    builtinDescriptor(Signal.subtle.Watcher.prototype, "constructor", true, true) &&
    builtinDescriptor(Signal.subtle.Watcher.prototype, "watch", true, true) &&
    builtinDescriptor(Signal.subtle.Watcher.prototype, "unwatch", true, true) &&
    builtinDescriptor(Signal.subtle.Watcher.prototype, "getPending", true, true);
var metadata = Signal.State.name === "State" && Signal.State.length === 1 &&
    Signal.State.prototype.get.name === "get" && Signal.State.prototype.get.length === 0 &&
    Signal.State.prototype.set.name === "set" && Signal.State.prototype.set.length === 1 &&
    Signal.Computed.name === "Computed" && Signal.Computed.length === 1 &&
    Signal.Computed.prototype.get.length === 0 &&
    Signal.subtle.Watcher.name === "Watcher" && Signal.subtle.Watcher.length === 1 &&
    Signal.subtle.Watcher.prototype.watch.length === 0 &&
    Signal.subtle.Watcher.prototype.unwatch.length === 0 &&
    Signal.subtle.Watcher.prototype.getPending.length === 0;
var newOnly = false;
try { Signal.State(1); } catch (error) { newOnly = error instanceof TypeError; }
try { Signal.Computed(function() {}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { Signal.subtle.Watcher(function() {}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
var state = new Signal.State(1);
var computedReceiver;
var computed = new Signal.Computed(function() { computedReceiver = this; return state.get(); });
var watcher = new Signal.subtle.Watcher(function() {});
var brands = Signal.State.prototype.get.call(state) === 1;
try { Signal.State.prototype.get.call(computed); brands = false; } catch (error) {
    brands = brands && error instanceof TypeError;
}
try { Signal.Computed.prototype.get.call(state); brands = false; } catch (error) {
    brands = brands && error instanceof TypeError;
}
try { Signal.subtle.Watcher.prototype.getPending.call(state); brands = false; } catch (error) {
    brands = brands && error instanceof TypeError;
}
var callbackReceiver = computed.get() === 1 && computedReceiver === computed;
class StateSubclass extends Signal.State {}
class ComputedSubclass extends Signal.Computed {}
var subclassState = new StateSubclass(4);
var subclassReceiver;
var subclassComputed = new ComputedSubclass(function() {
    subclassReceiver = this;
    return subclassState.get() + 1;
});
var subclasses = Object.getPrototypeOf(subclassState) === StateSubclass.prototype &&
    subclassState instanceof Signal.State && subclassState.get() === 4 &&
    Object.getPrototypeOf(subclassComputed) === ComputedSubclass.prototype &&
    subclassComputed instanceof Signal.Computed && subclassComputed.get() === 5 &&
    subclassReceiver === subclassComputed;
function StateTarget() {}
StateTarget.prototype = { marker: 1 };
var redirected = Reflect.construct(Signal.State, [7], StateTarget);
var newTarget = Object.getPrototypeOf(redirected) === StateTarget.prototype &&
    Signal.State.prototype.get.call(redirected) === 7;
globalDescriptor && namespaceDescriptors && prototypeDescriptors && metadata && newOnly && brands &&
callbackReceiver && subclasses && newTarget && Object.getPrototypeOf(Signal) === Object.prototype &&
Object.getPrototypeOf(Signal.subtle) === Object.prototype;
"#;

const SIGNAL_CROSS_REALM_SOURCE: &str = r#"
var identities = foreignSignal !== Signal && foreignSignal.State !== Signal.State &&
    foreignSignal.State.prototype !== Signal.State.prototype &&
    foreignSignal.Computed !== Signal.Computed &&
    foreignSignal.subtle.Watcher !== Signal.subtle.Watcher &&
    foreignSignal.subtle.watched !== Signal.subtle.watched &&
    foreignSignal.subtle.unwatched !== Signal.subtle.unwatched;
var local = new Signal.State(2);
var foreign = new foreignSignal.State(3);
var crossBrand = Signal.State.prototype.get.call(foreign) === 3 &&
    foreignSignal.State.prototype.get.call(local) === 2;
class ForeignStateSubclass extends foreignSignal.State {}
var subclass = new ForeignStateSubclass(9);
var subclassed = Object.getPrototypeOf(subclass) === ForeignStateSubclass.prototype &&
    subclass instanceof foreignSignal.State && Signal.State.prototype.get.call(subclass) === 9;
var callbackReceiver;
class ForeignComputedSubclass extends foreignSignal.Computed {}
var computed = new ForeignComputedSubclass(function() { callbackReceiver = this; return foreign.get(); });
identities && crossBrand && subclassed && computed.get() === 3 && callbackReceiver === computed;
"#;

const SIGNAL_OPTIONS_SOURCE: &str = r#"
var trace = "";
var equalsThis = false;
var watchedThis = false;
var unwatchedThis = false;
var options = {};
Object.defineProperty(options, "equals", { get: function() {
    trace = trace + "e";
    return function(oldValue, newValue) {
        trace = trace + "q";
        equalsThis = this === state;
        return Object.is(oldValue, newValue);
    };
} });
Object.defineProperty(options, Signal.subtle.watched, { get: function() {
    trace = trace + "w";
    return function() { trace = trace + "W"; watchedThis = this === state; };
} });
Object.defineProperty(options, Signal.subtle.unwatched, { get: function() {
    trace = trace + "u";
    return function() { trace = trace + "U"; unwatchedThis = this === state; };
} });
var state = new Signal.State(1, options);
var defaultState = new Signal.State(NaN);
var defaultCalls = 0;
var defaultComputed = new Signal.Computed(function() {
    defaultCalls = defaultCalls + 1;
    return defaultState.get();
});
defaultComputed.get();
defaultState.set(NaN);
defaultComputed.get();
var zeroState = new Signal.State(-0);
var zeroCalls = 0;
var zeroComputed = new Signal.Computed(function() {
    zeroCalls = zeroCalls + 1;
    return zeroState.get();
});
zeroComputed.get();
zeroState.set(0);
zeroComputed.get();
state.set(1);
state.set(2);
var watcher = new Signal.subtle.Watcher(function() {});
var watcher2 = new Signal.subtle.Watcher(function() {});
watcher.watch(state);
watcher2.watch(state);
watcher.unwatch(state);
watcher2.unwatch(state);
trace === "ewuqqWU" && equalsThis && watchedThis && unwatchedThis &&
defaultCalls === 1 && zeroCalls === 2 && defaultState.get() !== defaultState.get();
"#;

const SIGNAL_HOOK_THROW_SOURCE: &str = r#"
var watchedCalls = 0;
var state = new Signal.State(1, { [Signal.subtle.watched]: function() {
    watchedCalls = watchedCalls + 1;
    if (watchedCalls === 1) throw 17;
} });
var watcher = new Signal.subtle.Watcher(function() {});
var first = false;
try { watcher.watch(state); } catch (error) { first = error === 17; }
var second = true;
try { watcher.watch(state); } catch (error) { second = false; }
first && second && watchedCalls === 1;
"#;

const SIGNAL_OPTIONS_ABRUPT_SOURCE: &str = r#"
var getterTrace = "";
var getterThrow = false;
try {
    new Signal.State(1, {
        get equals() { getterTrace = getterTrace + "e"; throw 13; },
        get [Signal.subtle.watched]() { getterTrace = getterTrace + "w"; },
        get [Signal.subtle.unwatched]() { getterTrace = getterTrace + "u"; }
    });
} catch (error) { getterThrow = error === 13; }
var equalsThis = false;
var state = new Signal.State(4, { equals: function() {
    equalsThis = this === state;
    throw 17;
} });
var equalsThrow = false;
try { state.set(5); } catch (error) { equalsThrow = error === 17; }
var nullOptions = new Signal.State(6, null);
getterThrow && getterTrace === "e" && equalsThrow && equalsThis && state.get() === 4 &&
nullOptions.get() === 6;
"#;

const SIGNAL_NOTIFY_SOURCE: &str = r#"
var state = new Signal.State(1);
var computed = new Signal.Computed(function() { return state.get() * 2; });
computed.get();
var trace = "";
var frozen = 0;
var watcher;
watcher = new Signal.subtle.Watcher(function() {
    trace = trace + "n";
    if (this !== watcher) frozen = -100;
    try { state.get(); } catch (error) { if (error instanceof TypeError) frozen++; }
    try { state.set(9); } catch (error) { if (error instanceof TypeError) frozen++; }
    try { watcher.watch(); } catch (error) { if (error instanceof TypeError) frozen++; }
});
watcher.watch(computed);
trace = trace + "b";
state.set(2);
trace = trace + "a";
var first = computed.get() === 4 && watcher.getPending()[0] === computed;
watcher.watch();
state.set(3);
var second = computed.get() === 6;
trace === "bnan" && frozen === 6 && first && second;
"#;

const SIGNAL_NOTIFY_ERRORS_SOURCE: &str = r#"
var state = new Signal.State(1);
var computed = new Signal.Computed(function() { return state.get(); });
computed.get();
var trace = "";
var first = new Signal.subtle.Watcher(function() { trace += "a"; throw 11; });
var second = new Signal.subtle.Watcher(function() { trace += "b"; throw 13; });
var third = new Signal.subtle.Watcher(function() { trace += "c"; });
first.watch(computed);
second.watch(computed);
third.watch(computed);
var aggregate = false;
try { state.set(2); } catch (error) {
    aggregate = error instanceof AggregateError && error.errors.length === 2 &&
        error.errors[0] === 11 && error.errors[1] === 13;
}
var other = new Signal.State(1);
var otherComputed = new Signal.Computed(function() { return other.get(); });
otherComputed.get();
var marker = {};
var single = new Signal.subtle.Watcher(function() { throw marker; });
single.watch(otherComputed);
var identity = false;
try { other.set(2); } catch (error) { identity = error === marker; }
trace === "abc" && aggregate && identity;
"#;

const SIGNAL_DYNAMIC_DEPENDENCIES_SOURCE: &str = r#"
var trace = "";
var selector = new Signal.State(true, {
    [Signal.subtle.watched]: function() { trace += "S"; }
});
var left = new Signal.State(1, {
    [Signal.subtle.watched]: function() { trace += "L"; },
    [Signal.subtle.unwatched]: function() { trace += "l"; }
});
var right = new Signal.State(2, {
    [Signal.subtle.watched]: function() { trace += "R"; }
});
var selected = new Signal.Computed(function() {
    return selector.get() ? left.get() : right.get();
});
selected.get();
var watcher = new Signal.subtle.Watcher(function() { trace += "N"; });
watcher.watch(selected);
selector.set(false);
var value = selected.get();
trace === "SLNlR" && value === 2;
"#;

#[test]
fn signal_namespace_is_installed_in_every_realm() {
    let mut isolate = test_isolate();
    assert!(isolate.realm.signal_namespace.is_some());
    let (realm, _) = isolate.create_realm().expect("child realm initializes");
    let child = isolate
        .inactive_realms
        .iter()
        .find(|(id, _)| *id == realm)
        .map(|(_, realm)| realm)
        .expect("child realm remains registered");
    assert!(child.signal_namespace.is_some());
    assert_ne!(child.signal_namespace, isolate.realm.signal_namespace);
}

#[test]
fn signal_graph_works_for_every_dispatch_batch() {
    assert_signal_source::<1>(false);
    assert_signal_source::<2>(false);
    assert_signal_source::<4>(false);
    assert_signal_source::<8>(false);
    assert_signal_source::<16>(false);
}

#[test]
fn signal_graph_edges_survive_forced_major_collection() {
    assert_signal_source::<8>(true);
}

#[test]
fn signal_api_contract_works_for_every_dispatch_batch() {
    assert_signal_api_contract::<1>(false);
    assert_signal_api_contract::<2>(false);
    assert_signal_api_contract::<4>(false);
    assert_signal_api_contract::<8>(false);
    assert_signal_api_contract::<16>(false);
}

#[test]
fn signal_api_contract_survives_forced_major_collection() {
    assert_signal_api_contract::<8>(true);
}

#[test]
fn signal_cross_realm_identity_and_calls_work_for_every_dispatch_batch() {
    assert_signal_cross_realm::<1>();
    assert_signal_cross_realm::<2>();
    assert_signal_cross_realm::<4>();
    assert_signal_cross_realm::<8>();
    assert_signal_cross_realm::<16>();
}

#[test]
fn signal_state_options_and_live_hooks_work_for_every_dispatch_batch() {
    assert_signal_options::<1>(false);
    assert_signal_options::<2>(false);
    assert_signal_options::<4>(false);
    assert_signal_options::<8>(false);
    assert_signal_options::<16>(false);
}

#[test]
fn signal_state_options_and_live_hooks_survive_forced_major_collection() {
    assert_signal_options::<8>(true);
}

#[test]
fn signal_hook_throw_preserves_graph_invariants() {
    let module = compile_signal_source(SIGNAL_HOOK_THROW_SOURCE, 8_650, "signals-hook-throw");
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<4>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("Signal hook throw fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

#[test]
fn signal_state_option_and_equals_abrupt_completion_restore_state() {
    let module = compile_signal_source(
        SIGNAL_OPTIONS_ABRUPT_SOURCE,
        8_675,
        "signals-options-abrupt",
    );
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("Signal options abrupt fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

#[test]
fn signal_notify_is_synchronous_and_frozen_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(SIGNAL_NOTIFY_SOURCE, 8_700, "signals-notify", false);
    assert_signal_behavior::<2>(SIGNAL_NOTIFY_SOURCE, 8_701, "signals-notify", false);
    assert_signal_behavior::<4>(SIGNAL_NOTIFY_SOURCE, 8_702, "signals-notify", false);
    assert_signal_behavior::<8>(SIGNAL_NOTIFY_SOURCE, 8_703, "signals-notify", false);
    assert_signal_behavior::<16>(SIGNAL_NOTIFY_SOURCE, 8_704, "signals-notify", false);
    assert_signal_behavior::<8>(SIGNAL_NOTIFY_SOURCE, 8_705, "signals-notify", true);
}

#[test]
fn signal_notify_runs_all_watchers_and_aggregates_errors() {
    assert_signal_behavior::<4>(
        SIGNAL_NOTIFY_ERRORS_SOURCE,
        8_710,
        "signals-notify-errors",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_NOTIFY_ERRORS_SOURCE,
        8_711,
        "signals-notify-errors",
        true,
    );
}

#[test]
fn signal_dynamic_dependency_diff_runs_lifecycle_hooks() {
    assert_signal_behavior::<1>(
        SIGNAL_DYNAMIC_DEPENDENCIES_SOURCE,
        8_720,
        "signals-dynamic-dependencies",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_DYNAMIC_DEPENDENCIES_SOURCE,
        8_721,
        "signals-dynamic-dependencies",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_DYNAMIC_DEPENDENCIES_SOURCE,
        8_722,
        "signals-dynamic-dependencies",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_DYNAMIC_DEPENDENCIES_SOURCE,
        8_723,
        "signals-dynamic-dependencies",
        true,
    );
}

/// Executes one Signals semantic fixture under a selected dispatch and collection policy.
fn assert_signal_behavior<const N: usize>(
    source: &'static str,
    source_id: u32,
    name: &'static str,
    forced_major: bool,
) {
    let module = compile_signal_source(source, source_id, name);
    let mut isolate = signal_api_test_isolate();
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
        .expect("Signal semantic fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Signal fixture {name}, dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes State options access, Object.is, custom equality, and live sink hooks.
fn assert_signal_options<const N: usize>(forced_major: bool) {
    let module = compile_signal_source(SIGNAL_OPTIONS_SOURCE, 8_600 + N as u32, "signals-options");
    let mut isolate = signal_api_test_isolate();
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
        .expect("Signal options fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Signal options dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes the shared native Signal graph under one dispatch and collection policy.
fn assert_signal_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(8_300 + N as u32),
                SourceName::new("signals-first-slice"),
                MediaType::JavaScript,
                Arc::from(SIGNAL_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Signal fixture compiles");
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
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("Signal fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Runs the descriptor, metadata, new-only, brand, subclass, and newTarget contract.
fn assert_signal_api_contract<const N: usize>(forced_major: bool) {
    let module = compile_signal_source(
        SIGNAL_API_CONTRACT_SOURCE,
        8_400 + N as u32,
        "signals-api-contract",
    );
    let mut isolate = signal_api_test_isolate();
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
        .expect("Signal API contract executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "API dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Gives the descriptor-heavy API fixture room without changing production isolate defaults.
fn signal_api_test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("Signal API test isolate initializes")
}

/// Injects one child Realm namespace and exercises foreign constructors and branded methods.
fn assert_signal_cross_realm<const N: usize>() {
    let module = compile_signal_source(
        SIGNAL_CROSS_REALM_SOURCE,
        8_500 + N as u32,
        "signals-cross-realm",
    );
    let mut isolate = test_isolate();
    let (_, child_global) = isolate.create_realm().expect("child Realm initializes");
    let signal_atom = isolate.intern_intrinsic_name(b"Signal").unwrap();
    let foreign_signal = isolate
        .get_data_property(child_global, signal_atom)
        .unwrap()
        .expect("child Realm publishes Signal");
    let foreign_atom = isolate.intern_intrinsic_name(b"foreignSignal").unwrap();
    let global = isolate
        .realm
        .global_object
        .expect("main global initializes");
    isolate
        .set_own_data_property(global, foreign_atom, foreign_signal)
        .unwrap();
    isolate.realm.set(foreign_atom, foreign_signal).unwrap();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .expect("cross-Realm Signal contract executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "cross-Realm dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_signal_source(
    source: &'static str,
    source_id: u32,
    name: &'static str,
) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new(name),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Signal fixture compiles")
}
