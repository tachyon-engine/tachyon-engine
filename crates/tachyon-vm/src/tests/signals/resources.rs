pub(super) const SIGNAL_NOTIFY_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceNotifyState = new Signal.State(0);
var resourceNotifyCalls = 0;
var resourceNotifyWatcher = new Signal.subtle.Watcher(function() { resourceNotifyCalls++; });
resourceNotifyWatcher.watch(resourceNotifyState);
true;
"#;

pub(super) const SIGNAL_NOTIFY_RESOURCE_FAIL_SOURCE: &str = "resourceNotifyState.set(1);";

pub(super) const SIGNAL_NOTIFY_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceNotifyWatcher.watch();
resourceNotifyState.set(2);
resourceNotifyCalls === 1 && resourceNotifyState.get() === 2;
"#;

pub(super) const SIGNAL_COMPUTED_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceComputedState = new Signal.State(1);
var resourceComputedCalls = 0;
var resourceComputed = new Signal.Computed(function() {
    resourceComputedCalls++;
    return resourceComputedState.get() + 1;
});
true;
"#;

pub(super) const SIGNAL_COMPUTED_RESOURCE_FAIL_SOURCE: &str = "resourceComputed.get();";

pub(super) const SIGNAL_COMPUTED_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceComputed.get() === 2 && resourceComputedCalls === 1 &&
Signal.subtle.currentComputed() === undefined;
"#;

pub(super) const SIGNAL_EQUALS_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceEqualsState = new Signal.State(1);
var resourceEqualsComputations = 0;
var resourceEqualsCalls = 0;
var resourceEqualsRead = Signal.State.prototype.get.bind(resourceEqualsState);
var resourceEquals = new Signal.Computed(function() {
    resourceEqualsComputations++;
    return resourceEqualsRead();
}, { equals: function(oldValue, newValue) {
    resourceEqualsCalls++;
    return oldValue === newValue;
} });
resourceEquals.get() === 1;
"#;

pub(super) const SIGNAL_EQUALS_RESOURCE_DIRTY_SOURCE: &str = "resourceEqualsState.set(2); true;";
pub(super) const SIGNAL_EQUALS_RESOURCE_FAIL_SOURCE: &str = "resourceEquals.get();";

pub(super) const SIGNAL_EQUALS_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceEquals.get() === 2 && resourceEqualsComputations === 2 && resourceEqualsCalls === 1 &&
Signal.subtle.currentComputed() === undefined;
"#;

pub(super) const SIGNAL_UNTRACK_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceUntrackCalls = 0;
var resourceUntrackOwner = false;
var resourceUntrack = new Signal.Computed(function() {
    resourceUntrackCalls++;
    resourceUntrackOwner = Signal.subtle.currentComputed() === resourceUntrack;
    return Signal.subtle.untrack(function() { return 7; });
});
true;
"#;

pub(super) const SIGNAL_UNTRACK_RESOURCE_FAIL_SOURCE: &str = "resourceUntrack.get();";

pub(super) const SIGNAL_UNTRACK_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceUntrack.get() === 7 && resourceUntrackCalls === 2 && resourceUntrackOwner &&
Signal.subtle.currentComputed() === undefined;
"#;

pub(super) const SIGNAL_STATE_EQUALS_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceStateEqualsCalls = 0;
var resourceStateEquals = new Signal.State(1, { equals: function(oldValue, newValue) {
    resourceStateEqualsCalls++;
    return oldValue === newValue;
} });
true;
"#;

pub(super) const SIGNAL_STATE_EQUALS_RESOURCE_FAIL_SOURCE: &str = "resourceStateEquals.set(2);";

pub(super) const SIGNAL_STATE_EQUALS_RESOURCE_RECOVER_SOURCE: &str = r#"
var resourceStateEqualsUnchanged = resourceStateEquals.get() === 1 &&
    resourceStateEqualsCalls === 0;
resourceStateEquals.set(2);
resourceStateEqualsUnchanged && resourceStateEquals.get() === 2 &&
    resourceStateEqualsCalls === 1;
"#;

pub(super) const SIGNAL_WATCHED_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceWatchedCalls = 0;
var resourceWatchedNotifyCalls = 0;
var resourceWatchedState = new Signal.State(0, {
    [Signal.subtle.watched]: function() { resourceWatchedCalls++; }
});
var resourceWatchedWatcher = new Signal.subtle.Watcher(function() {
    resourceWatchedNotifyCalls++;
});
true;
"#;

pub(super) const SIGNAL_WATCHED_RESOURCE_FAIL_SOURCE: &str =
    "resourceWatchedWatcher.watch(resourceWatchedState);";

pub(super) const SIGNAL_WATCHED_RESOURCE_RECOVER_SOURCE: &str = r#"
var resourceWatchedAttached = Signal.subtle.introspectSources(resourceWatchedWatcher).length === 1 &&
    resourceWatchedCalls === 0;
resourceWatchedWatcher.watch(resourceWatchedState);
resourceWatchedState.set(1);
resourceWatchedAttached && resourceWatchedCalls === 0 && resourceWatchedNotifyCalls === 1;
"#;

pub(super) const SIGNAL_UNWATCHED_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceUnwatchedWatchedCalls = 0;
var resourceUnwatchedCalls = 0;
var resourceUnwatchedNotifyCalls = 0;
var resourceUnwatchedState = new Signal.State(0, {
    [Signal.subtle.watched]: function() { resourceUnwatchedWatchedCalls++; },
    [Signal.subtle.unwatched]: function() { resourceUnwatchedCalls++; }
});
var resourceUnwatchedWatcher = new Signal.subtle.Watcher(function() {
    resourceUnwatchedNotifyCalls++;
});
resourceUnwatchedWatcher.watch(resourceUnwatchedState);
resourceUnwatchedWatchedCalls === 1;
"#;

pub(super) const SIGNAL_UNWATCHED_RESOURCE_FAIL_SOURCE: &str =
    "resourceUnwatchedWatcher.unwatch(resourceUnwatchedState);";

pub(super) const SIGNAL_UNWATCHED_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceUnwatchedState.set(1);
var resourceUnwatchedDetached = Signal.subtle.introspectSources(resourceUnwatchedWatcher).length === 0 &&
    resourceUnwatchedCalls === 0 && resourceUnwatchedNotifyCalls === 0;
resourceUnwatchedWatcher.watch(resourceUnwatchedState);
resourceUnwatchedState.set(2);
resourceUnwatchedDetached && resourceUnwatchedWatchedCalls === 2 &&
    resourceUnwatchedCalls === 0 && resourceUnwatchedNotifyCalls === 1;
"#;

#[derive(Clone, Copy)]
pub(super) struct SignalResourceCase {
    pub(super) setup: &'static str,
    pub(super) failure: &'static str,
    pub(super) recovery: &'static str,
    pub(super) dirty: Option<&'static str>,
    pub(super) completion_limit: u32,
    pub(super) expects_stack_limit: bool,
    pub(super) source_id: u32,
    pub(super) name: &'static str,
}

pub(super) const PINNED_PROPOSAL_FIXTURES: &[(&str, &str)] = &[
    (
        "guards",
        include_str!("../../../../../tests/proposal-signals/guards.js"),
    ),
    (
        "state-computed",
        include_str!("../../../../../tests/proposal-signals/state-computed.js"),
    ),
    (
        "custom-equality-errors",
        include_str!("../../../../../tests/proposal-signals/custom-equality-errors.js"),
    ),
    (
        "cycles-pruning",
        include_str!("../../../../../tests/proposal-signals/cycles-pruning.js"),
    ),
    (
        "dynamic-graph",
        include_str!("../../../../../tests/proposal-signals/dynamic-graph.js"),
    ),
    (
        "watcher-liveness",
        include_str!("../../../../../tests/proposal-signals/watcher-liveness.js"),
    ),
    (
        "receivers-frozen",
        include_str!("../../../../../tests/proposal-signals/receivers-frozen.js"),
    ),
    (
        "untrack-introspection-brands",
        include_str!("../../../../../tests/proposal-signals/untrack-introspection-brands.js"),
    ),
    (
        "ported-graph-regressions",
        include_str!("../../../../../tests/proposal-signals/ported-graph-regressions.js"),
    ),
    (
        "graph-convergence-order",
        include_str!("../../../../../tests/proposal-signals/graph-convergence-order.js"),
    ),
    (
        "watcher-dynamic-dependencies",
        include_str!("../../../../../tests/proposal-signals/watcher-dynamic-dependencies.js"),
    ),
];
