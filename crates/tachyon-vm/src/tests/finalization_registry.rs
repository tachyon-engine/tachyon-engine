use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const FINALIZATION_REGISTRY_SOURCE: &str = r#"
var callback = function (_) {};
var registry = new FinalizationRegistry(callback);
var first = {};
var second = {};
var objectToken = {};
var symbolToken = Symbol("token");
registry.register(first, "first", objectToken);
registry.register(second, 42, objectToken);
var removedBoth = registry.unregister(objectToken);
registry.register(first, undefined, symbolToken);
var removedSymbol = registry.unregister(symbolToken);
var removedAgain = registry.unregister(symbolToken);
var primitiveTargetRejected = false;
var registeredSymbolRejected = false;
var sameValueRejected = false;
var callbackRejected = false;
var callRejected = false;
var registerBrandRejected = false;
var unregisterBrandRejected = false;
try { registry.register(1, "held"); } catch (error) { primitiveTargetRejected = error instanceof TypeError; }
try { registry.register(Symbol.for("registered"), "held"); } catch (error) { registeredSymbolRejected = error instanceof TypeError; }
try { registry.register(first, first); } catch (error) { sameValueRejected = error instanceof TypeError; }
try { new FinalizationRegistry(1); } catch (error) { callbackRejected = error instanceof TypeError; }
try { FinalizationRegistry(callback); } catch (error) { callRejected = error instanceof TypeError; }
try { FinalizationRegistry.prototype.register.call({}, first, "held"); } catch (error) { registerBrandRejected = error instanceof TypeError; }
try { FinalizationRegistry.prototype.unregister.call({}, objectToken); } catch (error) { unregisterBrandRejected = error instanceof TypeError; }
var tagDescriptor = Object.getOwnPropertyDescriptor(FinalizationRegistry.prototype, Symbol.toStringTag);
removedBoth && removedSymbol && !removedAgain && primitiveTargetRejected &&
registeredSymbolRejected && sameValueRejected && callbackRejected && callRejected &&
registerBrandRejected && unregisterBrandRejected &&
FinalizationRegistry.name === "FinalizationRegistry" && FinalizationRegistry.length === 1 &&
FinalizationRegistry.prototype.register.name === "register" &&
FinalizationRegistry.prototype.register.length === 2 &&
FinalizationRegistry.prototype.unregister.name === "unregister" &&
FinalizationRegistry.prototype.unregister.length === 1 &&
Object.getPrototypeOf(registry) === FinalizationRegistry.prototype &&
Object.prototype.toString.call(registry) === "[object FinalizationRegistry]" &&
tagDescriptor.value === "FinalizationRegistry" && tagDescriptor.writable === false &&
tagDescriptor.enumerable === false && tagDescriptor.configurable === true;
"#;

#[test]
fn finalization_registry_surface_works_for_every_dispatch_batch() {
    assert_finalization_registry_source::<1>(false);
    assert_finalization_registry_source::<2>(false);
    assert_finalization_registry_source::<4>(false);
    assert_finalization_registry_source::<8>(false);
    assert_finalization_registry_source::<16>(false);
}

#[test]
fn finalization_registry_edges_survive_forced_major_collection() {
    assert_finalization_registry_source::<8>(true);
}

#[test]
fn dead_target_schedules_javascript_cleanup_callback() {
    let mut isolate = test_isolate();
    execute_source(
        &mut isolate,
        7_451,
        r#"
var cleanupValue;
var heldValue = { marker: 42 };
var cleanupRegistry = new FinalizationRegistry(function (held) { cleanupValue = held; });
var cleanupTarget = {};
cleanupRegistry.register(cleanupTarget, heldValue);
"#,
    );
    execute_source(&mut isolate, 7_452, "cleanupTarget = null;");
    isolate.heap.clear_kept_objects_at_job_boundary();
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate.heap.collect_major(&mut roots).unwrap();
    execute_source(&mut isolate, 7_453, "0;");
    let outcome = execute_source(
        &mut isolate,
        7_454,
        "cleanupValue === heldValue && cleanupValue.marker === 42;",
    );
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn cleanup_throw_is_reported_after_consuming_the_job() {
    let mut isolate = test_isolate();
    execute_source(
        &mut isolate,
        7_455,
        r#"
var throwingRegistry = new FinalizationRegistry(function (_) { throw 77; });
var throwingTarget = {};
throwingRegistry.register(throwingTarget, "held");
"#,
    );
    execute_source(&mut isolate, 7_456, "throwingTarget = null;");
    isolate.heap.clear_kept_objects_at_job_boundary();
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate.heap.collect_major(&mut roots).unwrap();
    let trigger = compile_fixture("0;", 7_457, "finalization-registry-throw");
    let outcome = isolate
        .execute(
            &trigger,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("cleanup callback throw is a JavaScript completion");
    assert!(matches!(outcome, RunOutcome::Thrown(value) if value.as_i32() == Some(77)));
    assert_eq!(isolate.finalization_job_queue_stats().queued, 0);
}

/// Runs the shared API fixture under one dispatch and collection policy.
fn assert_finalization_registry_source<const N: usize>(forced_major: bool) {
    let module = compile_fixture(FINALIZATION_REGISTRY_SOURCE, 7_450, "finalization-registry");
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
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("FinalizationRegistry fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles and executes one source in the isolate's existing realm.
fn execute_source(isolate: &mut Isolate, source_id: u32, source: &str) -> RunOutcome {
    let module = compile_fixture(source, source_id, "finalization-registry-lifecycle");
    isolate
        .execute(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("FinalizationRegistry lifecycle fixture executes")
}

/// Compiles one FinalizationRegistry fixture independently of dispatch policy.
fn compile_fixture(source: &str, source_id: u32, name: &str) -> CompiledModule {
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
        .expect("FinalizationRegistry fixture compiles")
}
