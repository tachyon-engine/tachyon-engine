use super::*;

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

/// Runs every pinned proposal fixture across all dispatch batches and a forced major collection.
#[test]
fn pinned_proposal_fixtures_cover_dispatch_and_forced_major() {
    for (index, (name, source)) in PINNED_PROPOSAL_FIXTURES.iter().enumerate() {
        let source_id = 9_000 + index as u32;
        assert_signal_behavior::<1>(source, source_id, name, false);
        assert_signal_behavior::<2>(source, source_id + 16, name, false);
        assert_signal_behavior::<4>(source, source_id + 32, name, false);
        assert_signal_behavior::<8>(source, source_id + 48, name, false);
        assert_signal_behavior::<16>(source, source_id + 64, name, false);
        assert_signal_behavior::<8>(source, source_id + 80, name, true);
    }
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
    assert_signal_cross_realm::<1>(false);
    assert_signal_cross_realm::<2>(false);
    assert_signal_cross_realm::<4>(false);
    assert_signal_cross_realm::<8>(false);
    assert_signal_cross_realm::<16>(false);
    assert_signal_cross_realm::<8>(true);
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
/// Verifies observable option failures restore constructor and equality callback state.
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

/// Covers nested untracking, abrupt restoration, frozen entry, and exact callback roots.
#[test]
fn signal_untrack_restores_dependency_ownership_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(SIGNAL_UNTRACK_SOURCE, 8_746, "signals-untrack", false);
    assert_signal_behavior::<2>(SIGNAL_UNTRACK_SOURCE, 8_747, "signals-untrack", false);
    assert_signal_behavior::<4>(SIGNAL_UNTRACK_SOURCE, 8_748, "signals-untrack", false);
    assert_signal_behavior::<8>(SIGNAL_UNTRACK_SOURCE, 8_749, "signals-untrack", false);
    assert_signal_behavior::<16>(SIGNAL_UNTRACK_SOURCE, 8_750, "signals-untrack", false);
    assert_signal_behavior::<8>(SIGNAL_UNTRACK_SOURCE, 8_751, "signals-untrack", true);
}

/// Covers pinned semantics that permit writes while a Computed callback owns tracking.
#[test]
fn signal_computed_callbacks_allow_state_writes_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(
        SIGNAL_COMPUTED_WRITE_SOURCE,
        8_900,
        "signals-computed-write",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_COMPUTED_WRITE_SOURCE,
        8_901,
        "signals-computed-write",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_COMPUTED_WRITE_SOURCE,
        8_902,
        "signals-computed-write",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_COMPUTED_WRITE_SOURCE,
        8_903,
        "signals-computed-write",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_COMPUTED_WRITE_SOURCE,
        8_904,
        "signals-computed-write",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_COMPUTED_WRITE_SOURCE,
        8_905,
        "signals-computed-write",
        true,
    );
}

/// Ensures every user callback position follows ECMAScript IsCallable semantics.
#[test]
fn signal_callable_proxies_work_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(
        SIGNAL_CALLABLE_PROXY_SOURCE,
        8_906,
        "signals-callable-proxy",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_CALLABLE_PROXY_SOURCE,
        8_907,
        "signals-callable-proxy",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_CALLABLE_PROXY_SOURCE,
        8_908,
        "signals-callable-proxy",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_CALLABLE_PROXY_SOURCE,
        8_909,
        "signals-callable-proxy",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_CALLABLE_PROXY_SOURCE,
        8_910,
        "signals-callable-proxy",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_CALLABLE_PROXY_SOURCE,
        8_911,
        "signals-callable-proxy",
        true,
    );
}

/// Covers agent-wide current owner visibility without exposing graph internals.
#[test]
fn signal_current_computed_tracks_nested_owners_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(
        SIGNAL_CURRENT_COMPUTED_SOURCE,
        8_752,
        "signals-current-computed",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_CURRENT_COMPUTED_SOURCE,
        8_753,
        "signals-current-computed",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_CURRENT_COMPUTED_SOURCE,
        8_754,
        "signals-current-computed",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_CURRENT_COMPUTED_SOURCE,
        8_755,
        "signals-current-computed",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_CURRENT_COMPUTED_SOURCE,
        8_756,
        "signals-current-computed",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_CURRENT_COMPUTED_SOURCE,
        8_757,
        "signals-current-computed",
        true,
    );
}

/// Covers ordered graph snapshots, live-edge visibility, brands, and frozen read-only access.
#[test]
fn signal_introspection_reports_ordered_live_graph_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(
        SIGNAL_INTROSPECTION_SOURCE,
        8_758,
        "signals-introspection",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_INTROSPECTION_SOURCE,
        8_759,
        "signals-introspection",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_INTROSPECTION_SOURCE,
        8_760,
        "signals-introspection",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_INTROSPECTION_SOURCE,
        8_761,
        "signals-introspection",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_INTROSPECTION_SOURCE,
        8_762,
        "signals-introspection",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_INTROSPECTION_SOURCE,
        8_763,
        "signals-introspection",
        true,
    );
}

/// Covers the pinned Watcher state machine, pending ordering, and idempotent membership.
#[test]
fn signal_watcher_state_machine_works_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(
        SIGNAL_WATCHER_STATE_SOURCE,
        8_715,
        "signals-watcher-state",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_WATCHER_STATE_SOURCE,
        8_716,
        "signals-watcher-state",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_WATCHER_STATE_SOURCE,
        8_717,
        "signals-watcher-state",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_WATCHER_STATE_SOURCE,
        8_718,
        "signals-watcher-state",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_WATCHER_STATE_SOURCE,
        8_719,
        "signals-watcher-state",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_WATCHER_STATE_SOURCE,
        8_714,
        "signals-watcher-state",
        true,
    );
}

#[test]
/// Verifies branch changes reconcile ordered live edges and lifecycle hooks.
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

/// Covers iterative pruning, shared diamonds, cached throws, invalidation, and cycles.
#[test]
fn signal_checked_pull_prunes_diamonds_and_caches_abrupt_completions() {
    assert_signal_behavior::<1>(
        SIGNAL_CHECKED_PULL_SOURCE,
        8_730,
        "signals-checked-pull",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_CHECKED_PULL_SOURCE,
        8_731,
        "signals-checked-pull",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_CHECKED_PULL_SOURCE,
        8_732,
        "signals-checked-pull",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_CHECKED_PULL_SOURCE,
        8_733,
        "signals-checked-pull",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_CHECKED_PULL_SOURCE,
        8_734,
        "signals-checked-pull",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_CHECKED_PULL_SOURCE,
        8_735,
        "signals-checked-pull",
        true,
    );
}

/// Covers Computed options order, custom equality, pruning, abrupt cache, and invalidation.
#[test]
fn signal_computed_custom_equals_works_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(
        SIGNAL_COMPUTED_EQUALS_SOURCE,
        8_740,
        "signals-computed-equals",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_COMPUTED_EQUALS_SOURCE,
        8_741,
        "signals-computed-equals",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_COMPUTED_EQUALS_SOURCE,
        8_742,
        "signals-computed-equals",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_COMPUTED_EQUALS_SOURCE,
        8_743,
        "signals-computed-equals",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_COMPUTED_EQUALS_SOURCE,
        8_744,
        "signals-computed-equals",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_COMPUTED_EQUALS_SOURCE,
        8_745,
        "signals-computed-equals",
        true,
    );
}

/// Covers Computed hook option order, liveness transitions, abrupt recovery, and dynamic sources.
#[test]
fn signal_computed_lifecycle_hooks_work_for_every_dispatch_batch() {
    assert_signal_behavior::<1>(
        SIGNAL_COMPUTED_HOOKS_SOURCE,
        8_750,
        "signals-computed-hooks",
        false,
    );
    assert_signal_behavior::<2>(
        SIGNAL_COMPUTED_HOOKS_SOURCE,
        8_751,
        "signals-computed-hooks",
        false,
    );
    assert_signal_behavior::<4>(
        SIGNAL_COMPUTED_HOOKS_SOURCE,
        8_752,
        "signals-computed-hooks",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_COMPUTED_HOOKS_SOURCE,
        8_753,
        "signals-computed-hooks",
        false,
    );
    assert_signal_behavior::<16>(
        SIGNAL_COMPUTED_HOOKS_SOURCE,
        8_754,
        "signals-computed-hooks",
        false,
    );
    assert_signal_behavior::<8>(
        SIGNAL_COMPUTED_HOOKS_SOURCE,
        8_755,
        "signals-computed-hooks",
        true,
    );
}

/// Covers every Signals callback boundary where completion quota failure must restore agent state.
#[test]
fn signal_resource_failures_restore_agent_state_for_every_dispatch_batch() {
    assert_signal_resource_restoration::<1>(false);
    assert_signal_resource_restoration::<2>(false);
    assert_signal_resource_restoration::<4>(false);
    assert_signal_resource_restoration::<8>(false);
    assert_signal_resource_restoration::<16>(false);
    assert_signal_resource_restoration::<8>(true);
}

/// Runs the allocation-heavy Signals entry points with a minor collection at every young allocation.
#[test]
fn signal_forced_minor_allocation_matrix_works_for_every_dispatch_batch() {
    for batch in [1, 2, 4, 8, 16] {
        match batch {
            1 => assert_signal_forced_minor::<1>(),
            2 => assert_signal_forced_minor::<2>(),
            4 => assert_signal_forced_minor::<4>(),
            8 => assert_signal_forced_minor::<8>(),
            16 => assert_signal_forced_minor::<16>(),
            _ => unreachable!("matrix only contains supported dispatch batches"),
        }
    }
}
