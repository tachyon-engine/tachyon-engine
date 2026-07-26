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
    builtinDescriptor(Signal.subtle, "Watcher", true, true);
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
    foreignSignal.subtle.Watcher !== Signal.subtle.Watcher;
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
