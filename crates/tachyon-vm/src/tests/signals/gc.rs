use super::*;

pub(super) fn assert_signal_forced_minor<const N: usize>() {
    let fixtures = [
        (SIGNAL_SOURCE, "signals-minor-core"),
        (SIGNAL_NOTIFY_SOURCE, "signals-minor-notify"),
        (SIGNAL_WATCHER_STATE_SOURCE, "signals-minor-watcher"),
        (
            SIGNAL_DYNAMIC_DEPENDENCIES_SOURCE,
            "signals-minor-recompute",
        ),
        (SIGNAL_INTROSPECTION_SOURCE, "signals-minor-introspection"),
    ];
    for (index, (source, name)) in fixtures.into_iter().enumerate() {
        let mut isolate = signal_minor_test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Minor);
        assert_signal_job::<N>(
            &mut isolate,
            source,
            9_500 + (N as u32) * 16 + index as u32,
            name,
        );
    }
}

/// Gives the forced-minor matrix enough bounded heap for descriptor-heavy fixtures.
fn signal_minor_test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(128 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("Signal forced-minor isolate initializes")
}

/// Exercises graph ownership across job boundaries and explicit major collections.
#[test]
fn signal_gc_liveness_contract_works_for_every_dispatch_batch() {
    assert_signal_gc_liveness::<1>(8_800);
    assert_signal_gc_liveness::<2>(8_810);
    assert_signal_gc_liveness::<4>(8_820);
    assert_signal_gc_liveness::<8>(8_830);
    assert_signal_gc_liveness::<16>(8_840);
}

/// Runs setup, collection, detach, and collection as distinct ECMAScript jobs.
fn assert_signal_gc_liveness<const N: usize>(source_id: u32) {
    let mut isolate = signal_api_test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    assert_signal_job::<N>(
        &mut isolate,
        SIGNAL_GC_LIVENESS_SETUP_SOURCE,
        source_id,
        "signals-gc-liveness-setup",
    );
    assert_signal_job::<N>(
        &mut isolate,
        SIGNAL_GC_LIVENESS_DROP_ROOTS_SOURCE,
        source_id + 1,
        "signals-gc-liveness-drop-roots",
    );
    collect_signal_major_at_job_boundary(&mut isolate);
    assert_signal_job::<N>(
        &mut isolate,
        SIGNAL_GC_LIVENESS_AFTER_FIRST_MAJOR_SOURCE,
        source_id + 2,
        "signals-gc-liveness-first-major",
    );
    assert_signal_job::<N>(
        &mut isolate,
        SIGNAL_GC_LIVENESS_DROP_RECOVERED_SOURCE,
        source_id + 3,
        "signals-gc-liveness-drop-recovered",
    );
    collect_signal_major_at_job_boundary(&mut isolate);
    assert_signal_job::<N>(
        &mut isolate,
        SIGNAL_GC_LIVENESS_AFTER_UNWATCH_MAJOR_SOURCE,
        source_id + 4,
        "signals-gc-liveness-unwatch-major",
    );
}

/// Clears the current-job kept-object set before tracing the complete VM root surface.
fn collect_signal_major_at_job_boundary(isolate: &mut Isolate) {
    isolate.heap.clear_kept_objects_at_job_boundary();
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate
        .heap
        .collect_major(&mut roots)
        .expect("Signal graph major collection succeeds");
}

/// Compiles and executes one assertion-valued Signals job with a selected dispatch batch.
pub(super) fn assert_signal_job<const N: usize>(
    isolate: &mut Isolate,
    source: &'static str,
    source_id: u32,
    name: &'static str,
) {
    let module = compile_signal_source(source, source_id, name);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .expect("Signal GC liveness job executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Signal GC liveness job {name}, dispatch batch {N} returned {outcome:?}"
    );
}
