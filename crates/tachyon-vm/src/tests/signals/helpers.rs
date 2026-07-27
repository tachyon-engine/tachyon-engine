use super::*;

/// Injects completion quota failures without allocator globals, then proves the isolate remains usable.
pub(super) fn assert_signal_resource_restoration<const N: usize>(forced_major: bool) {
    for case in [
        SignalResourceCase {
            setup: SIGNAL_NOTIFY_RESOURCE_SETUP_SOURCE,
            failure: SIGNAL_NOTIFY_RESOURCE_FAIL_SOURCE,
            recovery: SIGNAL_NOTIFY_RESOURCE_RECOVER_SOURCE,
            dirty: None,
            completion_limit: 0,
            expects_stack_limit: true,
            source_id: 9_200,
            name: "notify",
        },
        SignalResourceCase {
            setup: SIGNAL_COMPUTED_RESOURCE_SETUP_SOURCE,
            failure: SIGNAL_COMPUTED_RESOURCE_FAIL_SOURCE,
            recovery: SIGNAL_COMPUTED_RESOURCE_RECOVER_SOURCE,
            dirty: None,
            completion_limit: 0,
            expects_stack_limit: false,
            source_id: 9_210,
            name: "computed",
        },
        SignalResourceCase {
            setup: SIGNAL_EQUALS_RESOURCE_SETUP_SOURCE,
            failure: SIGNAL_EQUALS_RESOURCE_FAIL_SOURCE,
            recovery: SIGNAL_EQUALS_RESOURCE_RECOVER_SOURCE,
            dirty: Some(SIGNAL_EQUALS_RESOURCE_DIRTY_SOURCE),
            completion_limit: 0,
            expects_stack_limit: false,
            source_id: 9_220,
            name: "equals",
        },
        SignalResourceCase {
            setup: SIGNAL_UNTRACK_RESOURCE_SETUP_SOURCE,
            failure: SIGNAL_UNTRACK_RESOURCE_FAIL_SOURCE,
            recovery: SIGNAL_UNTRACK_RESOURCE_RECOVER_SOURCE,
            dirty: None,
            completion_limit: 1,
            expects_stack_limit: true,
            source_id: 9_230,
            name: "untrack",
        },
    ] {
        assert_signal_resource_case::<N>(forced_major, case);
    }
}

/// Runs setup, a quota-terminated callback dispatch, and a fresh recovery job on one isolate.
fn assert_signal_resource_case<const N: usize>(forced_major: bool, case: SignalResourceCase) {
    let mut isolate = signal_api_test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    assert_signal_job::<N>(
        &mut isolate,
        case.setup,
        case.source_id,
        "signals-resource-setup",
    );
    if let Some(dirty) = case.dirty {
        assert_signal_job::<N>(
            &mut isolate,
            dirty,
            case.source_id + 1,
            "signals-resource-dirty",
        );
    }
    isolate.stack_limits = StackLimits::new(64, 4_096).with_max_completions(case.completion_limit);
    let failure =
        compile_signal_source(case.failure, case.source_id + 2, "signals-resource-failure");
    let error = isolate
        .execute_with_batch::<N>(
            &failure,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect_err("Signal callback dispatch must hit the injected completion quota");
    let expected_error = if case.expects_stack_limit {
        matches!(error, ExecutionError::CompletionStackLimit { .. })
    } else {
        error == ExecutionError::CompletionAllocationFailed
    };
    assert!(
        expected_error,
        "dispatch batch {N}, forced_major={forced_major} returned {error:?}"
    );
    assert!(
        !isolate.signal_runtime.frozen,
        "{} left Signals frozen",
        case.name
    );
    assert!(
        isolate.signal_runtime.computing.is_none(),
        "{} leaked its dependency owner",
        case.name
    );
    isolate.stack_limits = StackLimits::new(64, 4_096);
    assert_signal_job::<N>(&mut isolate, case.recovery, case.source_id + 3, case.name);
}

/// Executes one Signals semantic fixture under a selected dispatch and collection policy.
pub(super) fn assert_signal_behavior<const N: usize>(
    source: &'static str,
    source_id: u32,
    name: &'static str,
    forced_major: bool,
) {
    assert_signal_behavior_with_collection::<N>(
        source,
        source_id,
        name,
        forced_major.then_some(ForcedCollectionMode::Major),
    );
}

/// Executes one pinned fixture under an explicit collector policy.
pub(super) fn assert_signal_behavior_with_collection<const N: usize>(
    source: &'static str,
    source_id: u32,
    name: &'static str,
    collection: Option<ForcedCollectionMode>,
) {
    let module = compile_signal_source(source, source_id, name);
    let mut isolate = if collection == Some(ForcedCollectionMode::Minor) {
        signal_minor_test_isolate()
    } else {
        signal_api_test_isolate()
    };
    if let Some(mode) = collection {
        isolate.heap.set_forced_collection_mode(mode);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .unwrap_or_else(|error| {
            panic!(
                "Signal fixture {name}, dispatch batch {N}, collection={collection:?} failed: {error:?}"
            )
        });
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Signal fixture {name}, dispatch batch {N}, collection={collection:?} returned {outcome:?}"
    );
}

/// Executes State options access, Object.is, custom equality, and live sink hooks.
pub(super) fn assert_signal_options<const N: usize>(forced_major: bool) {
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
pub(super) fn assert_signal_source<const N: usize>(forced_major: bool) {
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
pub(super) fn assert_signal_api_contract<const N: usize>(forced_major: bool) {
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
pub(super) fn signal_api_test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("Signal API test isolate initializes")
}

/// Exercises foreign constructors, subclasses, options keys, brands, cycles, and exceptions.
pub(super) fn assert_signal_cross_realm<const N: usize>(forced_major: bool) {
    let module = compile_signal_source(
        SIGNAL_CROSS_REALM_SOURCE,
        8_500 + N as u32,
        "signals-cross-realm",
    );
    let mut isolate = test_isolate();
    let (_, child_global) = isolate.create_realm().expect("child Realm initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
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
        "cross-Realm dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

pub(super) fn compile_signal_source(
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
