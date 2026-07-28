pub(super) const SIGNAL_OPTIONS_SOURCE: &str = r#"
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

pub(super) const SIGNAL_HOOK_THROW_SOURCE: &str = r#"
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

pub(super) const SIGNAL_OPTIONS_ABRUPT_SOURCE: &str = r#"
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

pub(super) const SIGNAL_NOTIFY_SOURCE: &str = r#"
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
    try { watcher.unwatch(computed); } catch (error) { if (error instanceof TypeError) frozen++; }
    try { watcher.getPending(); } catch (error) { if (error instanceof TypeError) frozen++; }
});
watcher.watch(computed);
trace = trace + "b";
state.set(2);
trace = trace + "a";
var pending = watcher.getPending();
var first = pending.length === 1 && pending[0] === computed && computed.get() === 4 &&
    watcher.getPending().length === 0;
watcher.watch();
state.set(3);
var second = computed.get() === 6;
trace === "bnan" && frozen === 10 && first && second;
"#;

pub(super) const SIGNAL_WATCHER_STATE_SOURCE: &str = r#"
var left = new Signal.State(1);
var right = new Signal.State(2);
var leftComputed = new Signal.Computed(function() { return left.get() * 2; });
var rightComputed = new Signal.Computed(function() { return right.get() * 3; });
var notifyCount = 0;
var watcher = new Signal.subtle.Watcher(function() { notifyCount++; });
watcher.watch(rightComputed, leftComputed, rightComputed);
var initial = watcher.getPending();
var initialOrder = initial.length === 2 && initial[0] === rightComputed &&
    initial[1] === leftComputed;
var rightValue = rightComputed.get() === 6;
var afterRight = watcher.getPending();
var settledOne = afterRight.length === 1 && afterRight[0] === leftComputed;
var leftValue = leftComputed.get() === 2 && watcher.getPending().length === 0;

left.set(4);
var changed = watcher.getPending();
var changedOrder = notifyCount === 1 && changed.length === 1 && changed[0] === leftComputed;
var changedValue = leftComputed.get() === 8 && watcher.getPending().length === 0;
left.set(5);
var waitingPending = watcher.getPending();
var waitingIgnored = notifyCount === 1 && waitingPending.length === 1 &&
    waitingPending[0] === leftComputed;
watcher.watch();
var rearmKeepsPending = watcher.getPending()[0] === leftComputed;
leftComputed.get();
left.set(6);
var rearmed = notifyCount === 2;

var stateNotifyCount = 0;
var stateWatcher = new Signal.subtle.Watcher(function() { stateNotifyCount++; });
stateWatcher.watch(right);
right.set(3);
var stateOnly = stateNotifyCount === 1 && stateWatcher.getPending().length === 0;
right.set(4);
var stateWaiting = stateNotifyCount === 1;
stateWatcher.watch();
right.set(5);
var stateRearmed = stateNotifyCount === 2 && stateWatcher.getPending().length === 0;

var watchedHooks = 0;
var unwatchedHooks = 0;
var hooked = new Signal.State(0, {
    [Signal.subtle.watched]: function() { watchedHooks++; },
    [Signal.subtle.unwatched]: function() { unwatchedHooks++; }
});
var duplicateWatcher = new Signal.subtle.Watcher(function() {});
duplicateWatcher.watch(hooked, hooked);
duplicateWatcher.unwatch(hooked, hooked);
var duplicateHooks = watchedHooks === 1 && unwatchedHooks === 1;

var unwatchLeft = new Signal.State(0);
var unwatchRight = new Signal.State(0);
var unwatchNotifyCount = 0;
var unwatchWatcher = new Signal.subtle.Watcher(function() { unwatchNotifyCount++; });
unwatchWatcher.watch(unwatchLeft, unwatchRight);
unwatchLeft.set(1);
unwatchWatcher.unwatch(unwatchLeft);
unwatchRight.set(1);
var unwatchDidNotRearm = unwatchNotifyCount === 1;
unwatchWatcher.watch();
unwatchRight.set(2);
var unwatchExplicitRearm = unwatchNotifyCount === 2;

var pendingSource = new Signal.State(1);
var pendingComputed = new Signal.Computed(function() { return pendingSource.get(); });
pendingComputed.get();
var pendingWatcher = new Signal.subtle.Watcher(function() {});
pendingWatcher.watch(pendingComputed);
pendingSource.set(2);
var pendingBeforeUnwatch = pendingWatcher.getPending()[0] === pendingComputed;
pendingWatcher.unwatch(pendingComputed);
var unwatchClears = pendingWatcher.getPending().length === 0;

var marker = {};
var throwCount = 0;
var throwState = new Signal.State(0);
var throwWatcher = new Signal.subtle.Watcher(function() { throwCount++; throw marker; });
throwWatcher.watch(throwState);
var firstIdentity = false;
try { throwState.set(1); } catch (error) { firstIdentity = error === marker; }
throwState.set(2);
var throwWaiting = throwCount === 1 && throwWatcher.getPending().length === 0;
throwWatcher.watch();
var secondIdentity = false;
try { throwState.set(3); } catch (error) { secondIdentity = error === marker; }
var throwRearmed = throwCount === 2;

initialOrder && rightValue && settledOne && leftValue && changedOrder && changedValue &&
waitingIgnored && rearmKeepsPending && rearmed && stateOnly && stateWaiting && stateRearmed &&
duplicateHooks && unwatchDidNotRearm && unwatchExplicitRearm && pendingBeforeUnwatch &&
unwatchClears && firstIdentity && throwWaiting && secondIdentity && throwRearmed;
"#;

pub(super) const SIGNAL_NOTIFY_ERRORS_SOURCE: &str = r#"
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

pub(super) const SIGNAL_DYNAMIC_DEPENDENCIES_SOURCE: &str = r#"
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

pub(super) const SIGNAL_CHECKED_PULL_SOURCE: &str = r#"
var source = new Signal.State(1);
var stableCalls = 0;
var stable = new Signal.Computed(function() {
    stableCalls++;
    source.get();
    return 5;
});
var middleCalls = 0;
var middle = new Signal.Computed(function() { middleCalls++; return stable.get() + 1; });
var topCalls = 0;
var top = new Signal.Computed(function() { topCalls++; return middle.get() + 1; });
var initialPruned = top.get() === 7 && stableCalls === 1 && middleCalls === 1 && topCalls === 1;
var watcher = new Signal.subtle.Watcher(function() {});
watcher.watch(top);
source.set(2);
var topPending = watcher.getPending();
var pruned = topPending.length === 1 && topPending[0] === top && top.get() === 7 &&
    watcher.getPending().length === 0 && stableCalls === 2 && middleCalls === 1 && topCalls === 1;

var diamondSource = new Signal.State(1);
var baseCalls = 0;
var base = new Signal.Computed(function() { baseCalls++; return diamondSource.get() * 2; });
var leftCalls = 0;
var left = new Signal.Computed(function() { leftCalls++; return base.get() + 1; });
var rightCalls = 0;
var right = new Signal.Computed(function() { rightCalls++; return base.get() + 2; });
var diamondCalls = 0;
var diamond = new Signal.Computed(function() {
    diamondCalls++;
    return left.get() + right.get();
});
var diamondWatcher = new Signal.subtle.Watcher(function() {});
diamondWatcher.watch(diamond);
var initialDiamond = diamond.get() === 7;
diamondSource.set(2);
var updatedDiamond = diamond.get() === 11 && baseCalls === 2 && leftCalls === 2 &&
    rightCalls === 2 && diamondCalls === 2;

var firstError = {};
var secondError = {};
var errorSource = new Signal.State(firstError);
var innerErrorCalls = 0;
var innerError = new Signal.Computed(function() {
    innerErrorCalls++;
    throw errorSource.get();
});
var outerErrorCalls = 0;
var outerError = new Signal.Computed(function() {
    outerErrorCalls++;
    return innerError.get();
});
var caughtFirst;
try { outerError.get(); } catch (error) { caughtFirst = error; }
var caughtCached;
try { outerError.get(); } catch (error) { caughtCached = error; }
errorSource.set(secondError);
var caughtSecond;
try { outerError.get(); } catch (error) { caughtSecond = error; }
var cachedErrors = caughtFirst === firstError && caughtCached === firstError &&
    caughtSecond === secondError && innerErrorCalls === 2 && outerErrorCalls === 2;

var cycle;
cycle = new Signal.Computed(function() { return cycle.get(); });
var cycleFirst;
var cycleSecond;
try { cycle.get(); } catch (error) { cycleFirst = error; }
try { cycle.get(); } catch (error) { cycleSecond = error; }
var cachedCycle = cycleFirst instanceof TypeError && cycleSecond === cycleFirst;

initialPruned && pruned && initialDiamond && updatedDiamond && cachedErrors && cachedCycle;
"#;

pub(super) const SIGNAL_COMPUTED_EQUALS_SOURCE: &str = r#"
var order = "";
var optionsRead = 0;
var orderedOptions = new Proxy({}, { get: function(target, key) {
    if (key === "equals") { optionsRead++; order += "g"; return undefined; }
} });
var badCallback = false;
try { new Signal.Computed(1, orderedOptions); } catch (error) {
    badCallback = error instanceof TypeError && optionsRead === 0;
}
var ordered = new Signal.Computed(function() { order += "c"; return 1; }, orderedOptions);
var constructorOrder = ordered.get() === 1 && order === "gc" && optionsRead === 1;

var getterMarker = {};
var abruptOptions = new Proxy({}, { get: function(target, key) {
    if (key === "equals") throw getterMarker;
} });
var getterIdentity = false;
try { new Signal.Computed(function() { return 1; }, abruptOptions); }
catch (error) { getterIdentity = error === getterMarker; }
var invalidTrace = "";
var callableCheck = false;
try {
    new Signal.Computed(function() { return 1; }, new Proxy({}, { get: function(target, key) {
        if (key === "equals") { invalidTrace += "e"; return 1; }
        invalidTrace += "w";
    } }));
} catch (error) { callableCheck = error instanceof TypeError && invalidTrace === "e"; }

var source = new Signal.State(1);
var computeCalls = 0;
var equalsCalls = 0;
var equalsThis = false;
var equalsArgs = false;
var stable = new Signal.Computed(function() { computeCalls++; source.get(); return 5; }, {
    equals: function(oldValue, newValue) {
        equalsCalls++;
        equalsThis = this === stable;
        equalsArgs = oldValue === 5 && newValue === 5;
        return true;
    }
});
var leftCalls = 0;
var left = new Signal.Computed(function() { leftCalls++; return stable.get() + 1; });
var rightCalls = 0;
var right = new Signal.Computed(function() { rightCalls++; return stable.get() + 2; });
var diamondCalls = 0;
var diamond = new Signal.Computed(function() { diamondCalls++; return left.get() + right.get(); });
var watcher = new Signal.subtle.Watcher(function() {});
watcher.watch(diamond);
var diamondInitial = diamond.get() === 13;
source.set(2);
var diamondPruned = diamond.get() === 13 && computeCalls === 2 && equalsCalls === 1 &&
    leftCalls === 1 && rightCalls === 1 && diamondCalls === 1 && equalsThis && equalsArgs;

var exact = new Signal.State(1);
var epsilon = new Signal.State(0.1);
var trackedInnerCalls = 0;
var trackedEqualsCalls = 0;
var trackedInner = new Signal.Computed(function() { trackedInnerCalls++; return exact.get(); }, {
    equals: function() { trackedEqualsCalls++; epsilon.get(); return true; }
});
var trackedOuterCalls = 0;
var trackedOuter = new Signal.Computed(function() { trackedOuterCalls++; return trackedInner.get(); });
var trackedWatcher = new Signal.subtle.Watcher(function() {});
trackedWatcher.watch(trackedOuter);
var trackedInitial = trackedOuter.get() === 1;
exact.set(2);
var trackedFirst = trackedOuter.get() === 1 && trackedInnerCalls === 2 &&
    trackedEqualsCalls === 1 && trackedOuterCalls === 1;
epsilon.set(0.2);
var trackedAgain = trackedOuter.get() === 1 && trackedInnerCalls === 3 &&
    trackedEqualsCalls === 2 && trackedOuterCalls === 1;

var throwSource = new Signal.State(1);
var throwMarker = {};
var throwComputeCalls = 0;
var throwEqualsCalls = 0;
var throwing = new Signal.Computed(function() { throwComputeCalls++; return throwSource.get(); }, {
    equals: function() { throwEqualsCalls++; throw throwMarker; }
});
var throwInitial = throwing.get() === 1;
throwSource.set(2);
var caughtFirst;
try { throwing.get(); } catch (error) { caughtFirst = error; }
var caughtCached;
try { throwing.get(); } catch (error) { caughtCached = error; }
throwSource.set(3);
var invalidated = throwing.get() === 3 && throwComputeCalls === 3 && throwEqualsCalls === 1;
var throwCache = throwInitial && caughtFirst === throwMarker && caughtCached === throwMarker;

badCallback && constructorOrder && getterIdentity && callableCheck && diamondInitial &&
diamondPruned && trackedInitial && trackedFirst && trackedAgain && throwCache && invalidated;
"#;

pub(super) const SIGNAL_COMPUTED_HOOKS_SOURCE: &str = r#"
var trace = "";
var watchedThis = false;
var unwatchedThis = false;
var source = new Signal.State(1, {
    [Signal.subtle.watched]: function() { trace += "s"; },
    [Signal.subtle.unwatched]: function() { trace += "t"; }
});
var options = new Proxy({}, { get: function(target, key) {
    if (key === "equals") { trace += "e"; return undefined; }
    if (key === Signal.subtle.watched) {
        trace += "w";
        return function() { trace += "C"; watchedThis = this === computed; };
    }
    if (key === Signal.subtle.unwatched) {
        trace += "u";
        return function() { trace += "D"; unwatchedThis = this === computed; };
    }
} });
var computed = new Signal.Computed(function() { return source.get(); }, options);
computed.get();
var first = new Signal.subtle.Watcher(function() {});
var second = new Signal.subtle.Watcher(function() {});
first.watch(computed);
second.watch(computed);
first.unwatch(computed);
var deduplicated = trace === "ewuCs" && Signal.subtle.hasSinks(computed) &&
    Signal.subtle.hasSinks(source);
second.unwatch(computed);
var lifecycle = trace === "ewuCsDt" && watchedThis && unwatchedThis &&
    !Signal.subtle.hasSinks(computed) && !Signal.subtle.hasSinks(source);

var getterMarker = {};
var abruptTrace = "";
var abrupt = false;
try {
    new Signal.Computed(function() { return 1; }, new Proxy({}, { get: function(target, key) {
        if (key === "equals") { abruptTrace += "e"; return undefined; }
        if (key === Signal.subtle.watched) { abruptTrace += "w"; throw getterMarker; }
        abruptTrace += "u";
    } }));
} catch (error) { abrupt = error === getterMarker && abruptTrace === "ew"; }

var hookMarker = {};
var throwCalls = 0;
var sourceWatched = 0;
var throwingSource = new Signal.State(2, {
    [Signal.subtle.watched]: function() { sourceWatched++; }
});
var throwing = new Signal.Computed(function() { return throwingSource.get(); }, {
    [Signal.subtle.watched]: function() { throwCalls++; throw hookMarker; }
});
throwing.get();
var throwingWatcher = new Signal.subtle.Watcher(function() {});
var hookIdentity = false;
try { throwingWatcher.watch(throwing); } catch (error) { hookIdentity = error === hookMarker; }
var invariant = hookIdentity && throwCalls === 1 && sourceWatched === 0 &&
    Signal.subtle.hasSinks(throwing) && Signal.subtle.hasSinks(throwingSource) &&
    Signal.subtle.introspectSinks(throwing)[0] === throwingWatcher;
throwingWatcher.watch(throwing);
throwingWatcher.unwatch(throwing);
var recovered = throwCalls === 1 && !Signal.subtle.hasSinks(throwing) &&
    !Signal.subtle.hasSinks(throwingSource);

var chooseLeft = new Signal.State(true);
var leftTrace = "";
var left = new Signal.State(3, {
    [Signal.subtle.watched]: function() { leftTrace += "L"; },
    [Signal.subtle.unwatched]: function() { leftTrace += "l"; }
});
var right = new Signal.State(4, {
    [Signal.subtle.watched]: function() { leftTrace += "R"; }
});
var branchHooks = 0;
var branch = new Signal.Computed(function() {
    return chooseLeft.get() ? left.get() : right.get();
}, { [Signal.subtle.watched]: function() { branchHooks++; } });
branch.get();
var branchWatcher = new Signal.subtle.Watcher(function() {});
branchWatcher.watch(branch);
chooseLeft.set(false);
var switched = branch.get() === 4 && branchHooks === 1 && leftTrace === "LlR";
branchWatcher.unwatch(branch);

deduplicated && lifecycle && abrupt && invariant && recovered && switched;
"#;

pub(super) const SIGNAL_GC_LIVENESS_SETUP_SOURCE: &str = r#"
var gcHookTrace = "";
var rootedSource = new Signal.State({ marker: 1 }, {
    [Signal.subtle.watched]: function() { gcHookTrace += "w"; },
    [Signal.subtle.unwatched]: function() { gcHookTrace += "u"; }
});
var transientWatcher = new Signal.subtle.Watcher(function() {});
transientWatcher.watch(rootedSource);
var weakWatcher = new WeakRef(transientWatcher);

var coldDependent = new Signal.Computed(function() { return rootedSource.get().marker; });
coldDependent.get();
var weakColdDependent = new WeakRef(coldDependent);

var coldComputed = new Signal.Computed(function() { return 1; });
var weakColdComputed = new WeakRef(coldComputed);

var cycleSource = new Signal.State(2, {
    [Signal.subtle.watched]: function() { gcHookTrace += "c"; },
    [Signal.subtle.unwatched]: function() { gcHookTrace += "x"; }
});
var cycleComputed = new Signal.Computed(function() { return cycleSource.get(); });
cycleComputed.get();
var cycleWatcher = new Signal.subtle.Watcher(function() {});
cycleWatcher.watch(cycleComputed);
var weakCycleSource = new WeakRef(cycleSource);
var weakCycleComputed = new WeakRef(cycleComputed);
var weakCycleWatcher = new WeakRef(cycleWatcher);
gcHookTrace === "wc";
"#;

pub(super) const SIGNAL_GC_LIVENESS_DROP_ROOTS_SOURCE: &str = r#"
transientWatcher = null;
coldDependent = null;
coldComputed = null;
cycleSource = null;
cycleComputed = null;
cycleWatcher = null;
true;
"#;

pub(super) const SIGNAL_GC_LIVENESS_AFTER_FIRST_MAJOR_SOURCE: &str = r#"
var recoveredWatcher = weakWatcher.deref();
var activeWatcherRetained = recoveredWatcher !== undefined;
var coldDependentCollected = weakColdDependent.deref() === undefined;
var coldComputedCollected = weakColdComputed.deref() === undefined;
var cycleSourceCollected = weakCycleSource.deref() === undefined;
var cycleComputedCollected = weakCycleComputed.deref() === undefined;
var cycleWatcherCollected = weakCycleWatcher.deref() === undefined;
var collectionSkippedHooks = gcHookTrace === "wc";
recoveredWatcher.unwatch(rootedSource);
recoveredWatcher = null;
activeWatcherRetained && coldDependentCollected && coldComputedCollected && cycleSourceCollected &&
cycleComputedCollected && cycleWatcherCollected && collectionSkippedHooks && gcHookTrace === "wcu";
"#;

pub(super) const SIGNAL_GC_LIVENESS_AFTER_UNWATCH_MAJOR_SOURCE: &str = r#"
weakWatcher.deref() === undefined && rootedSource.get().marker === 1 && gcHookTrace === "wcu";
"#;

pub(super) const SIGNAL_GC_LIVENESS_DROP_RECOVERED_SOURCE: &str = r#"
recoveredWatcher = null;
true;
"#;
