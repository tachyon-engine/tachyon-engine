use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const WEAK_REF_SOURCE: &str = r#"
var target = { marker: 1 };
var reference = new WeakRef(target);
var symbol = Symbol("local");
var symbolReference = new WeakRef(symbol);
var wellKnownReference = new WeakRef(Symbol.iterator);
var primitiveRejected = false;
var registeredRejected = false;
var callRejected = false;
var brandRejected = false;
try { new WeakRef(1); } catch (error) { primitiveRejected = error instanceof TypeError; }
try { new WeakRef(Symbol.for("registered")); } catch (error) { registeredRejected = error instanceof TypeError; }
try { WeakRef(target); } catch (error) { callRejected = error instanceof TypeError; }
try { WeakRef.prototype.deref.call({}); } catch (error) { brandRejected = error instanceof TypeError; }
var tagDescriptor = Object.getOwnPropertyDescriptor(WeakRef.prototype, Symbol.toStringTag);
reference.deref() === target && symbolReference.deref() === symbol &&
wellKnownReference.deref() === Symbol.iterator && primitiveRejected && registeredRejected &&
callRejected && brandRejected && WeakRef.name === "WeakRef" && WeakRef.length === 1 &&
WeakRef.prototype.deref.name === "deref" && WeakRef.prototype.deref.length === 0 &&
Object.getPrototypeOf(reference) === WeakRef.prototype &&
Object.prototype.toString.call(reference) === "[object WeakRef]" &&
tagDescriptor.value === "WeakRef" && tagDescriptor.writable === false &&
tagDescriptor.enumerable === false && tagDescriptor.configurable === true;
"#;

#[test]
fn weak_ref_surface_works_for_every_dispatch_batch() {
    assert_weak_ref_source::<1>(false);
    assert_weak_ref_source::<2>(false);
    assert_weak_ref_source::<4>(false);
    assert_weak_ref_source::<8>(false);
    assert_weak_ref_source::<16>(false);
}

#[test]
fn weak_ref_edges_survive_forced_major_during_the_current_job() {
    assert_weak_ref_source::<8>(true);
}

#[test]
fn weak_ref_target_clears_after_job_boundary_and_major_collection() {
    let mut isolate = test_isolate();
    execute_source(
        &mut isolate,
        7_441,
        "var collectedTarget = {}; var collectedReference = new WeakRef(collectedTarget);",
    );
    execute_source(&mut isolate, 7_442, "collectedTarget = null;");
    isolate.heap.clear_kept_objects_at_job_boundary();
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate.heap.collect_major(&mut roots).unwrap();
    let outcome = execute_source(
        &mut isolate,
        7_443,
        "collectedReference.deref() === undefined;",
    );
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Executes WeakRef target validation, metadata, branding, and dereference under one policy.
fn assert_weak_ref_source<const N: usize>(forced_major: bool) {
    let module = compile_weak_ref_fixture();
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
        .expect("WeakRef fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the shared fixture independently of dispatch and collection policy.
fn compile_weak_ref_fixture() -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_440),
                SourceName::new("weak-ref-fixture"),
                MediaType::JavaScript,
                Arc::from(WEAK_REF_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("WeakRef fixture compiles")
}

/// Compiles and executes one small script in the same realm for lifecycle tests.
fn execute_source(isolate: &mut Isolate, source_id: u32, source: &str) -> RunOutcome {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("weak-ref-lifecycle"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("WeakRef lifecycle fixture compiles");
    isolate
        .execute(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("WeakRef lifecycle fixture executes")
}
