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
