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
    builtinDescriptor(Signal.subtle, "untrack", true, true) &&
    builtinDescriptor(Signal.subtle, "currentComputed", true, true) &&
    builtinDescriptor(Signal.subtle, "introspectSources", true, true) &&
    builtinDescriptor(Signal.subtle, "introspectSinks", true, true) &&
    builtinDescriptor(Signal.subtle, "hasSources", true, true) &&
    builtinDescriptor(Signal.subtle, "hasSinks", true, true) &&
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
    Signal.subtle.Watcher.prototype.getPending.length === 0 &&
    Signal.subtle.untrack.name === "untrack" && Signal.subtle.untrack.length === 1 &&
    Signal.subtle.currentComputed.name === "currentComputed" &&
    Signal.subtle.currentComputed.length === 0 &&
    Signal.subtle.introspectSources.name === "introspectSources" &&
    Signal.subtle.introspectSources.length === 1 &&
    Signal.subtle.introspectSinks.name === "introspectSinks" &&
    Signal.subtle.introspectSinks.length === 1 &&
    Signal.subtle.hasSources.name === "hasSources" && Signal.subtle.hasSources.length === 1 &&
    Signal.subtle.hasSinks.name === "hasSinks" && Signal.subtle.hasSinks.length === 1;
var newOnly = false;
try { Signal.State(1); } catch (error) { newOnly = error instanceof TypeError; }
try { Signal.Computed(function() {}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { Signal.subtle.Watcher(function() {}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.subtle.untrack(function() {}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.subtle.currentComputed(); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.subtle.introspectSources({}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.subtle.introspectSinks({}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.subtle.hasSources({}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.subtle.hasSinks({}); newOnly = false; } catch (error) {
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

const SIGNAL_INTROSPECTION_SOURCE: &str = r#"
var select = new Signal.State(true);
var left = new Signal.State(2);
var right = new Signal.State(3);
var computed = new Signal.Computed(function() {
    select.get();
    if (select.get()) {
        left.get();
        right.get();
        return left.get();
    }
    right.get();
    left.get();
    return right.get();
});
var watcher = new Signal.subtle.Watcher(function() {});
var beforeSources = Signal.subtle.introspectSources(computed);
var before = beforeSources.length === 0 && !Signal.subtle.hasSources(computed) &&
    !Signal.subtle.hasSinks(computed) && !Signal.subtle.hasSinks(left);
watcher.watch(computed);
var value = computed.get() === 2;
var sources = Signal.subtle.introspectSources(computed);
var watched = Signal.subtle.introspectSources(watcher);
var sourceOrder = sources.length === 3 && sources[0] === select && sources[1] === left &&
    sources[2] === right && watched.length === 1 && watched[0] === computed;
var sinkOrder = Signal.subtle.introspectSinks(select).length === 1 &&
    Signal.subtle.introspectSinks(select)[0] === computed &&
    Signal.subtle.introspectSinks(left)[0] === computed &&
    Signal.subtle.introspectSinks(right)[0] === computed &&
    Signal.subtle.introspectSinks(computed)[0] === watcher &&
    Signal.subtle.hasSinks(select) && Signal.subtle.hasSinks(computed) &&
    Signal.subtle.hasSources(computed) && Signal.subtle.hasSources(watcher);
sources[0] = right;
watched[0] = left;
var freshSources = Signal.subtle.introspectSources(computed);
var freshWatched = Signal.subtle.introspectSources(watcher);
var fresh = freshSources !== sources && freshWatched !== watched &&
    freshSources[0] === select && freshWatched[0] === computed;

select.set(false);
var switched = computed.get() === 3;
var switchedSources = Signal.subtle.introspectSources(computed);
var switchedOrder = switchedSources.length === 3 && switchedSources[0] === select &&
    switchedSources[1] === right && switchedSources[2] === left;
watcher.unwatch(computed);
var detached = Signal.subtle.introspectSinks(select).length === 0 &&
    Signal.subtle.introspectSinks(left).length === 0 &&
    Signal.subtle.introspectSinks(right).length === 0 &&
    Signal.subtle.introspectSinks(computed).length === 0 &&
    !Signal.subtle.hasSinks(select) && !Signal.subtle.hasSinks(computed) &&
    Signal.subtle.hasSources(computed) && !Signal.subtle.hasSources(watcher);

var frozenRejections = 0;
var directWatcher = new Signal.subtle.Watcher(function() {
    try { Signal.subtle.introspectSources(directWatcher); } catch (error) {
        if (error instanceof TypeError) frozenRejections++;
    }
    try { Signal.subtle.introspectSinks(left); } catch (error) {
        if (error instanceof TypeError) frozenRejections++;
    }
    try { Signal.subtle.hasSources(directWatcher); } catch (error) {
        if (error instanceof TypeError) frozenRejections++;
    }
    try { Signal.subtle.hasSinks(left); } catch (error) {
        if (error instanceof TypeError) frozenRejections++;
    }
});
directWatcher.watch(left);
left.set(4);
directWatcher.unwatch(left);

var brands = 0;
try { Signal.subtle.introspectSources(left); } catch (error) {
    if (error instanceof TypeError) brands++;
}
try { Signal.subtle.introspectSources({}); } catch (error) {
    if (error instanceof TypeError) brands++;
}
try { Signal.subtle.hasSources(left); } catch (error) {
    if (error instanceof TypeError) brands++;
}
try { Signal.subtle.introspectSinks(watcher); } catch (error) {
    if (error instanceof TypeError) brands++;
}
try { Signal.subtle.hasSinks(watcher); } catch (error) {
    if (error instanceof TypeError) brands++;
}
try { Signal.subtle.hasSinks(); } catch (error) {
    if (error instanceof TypeError) brands++;
}
before && value && sourceOrder && sinkOrder && fresh && switched && switchedOrder && detached &&
frozenRejections === 4 && brands === 6;
"#;

const SIGNAL_CURRENT_COMPUTED_SOURCE: &str = r#"
var topLevel = Signal.subtle.currentComputed() === undefined;
var tracked = new Signal.State(1);
var hidden = new Signal.State(10);
var innerSeen = false;
var inner;
inner = new Signal.Computed(function() {
    innerSeen = Signal.subtle.currentComputed() === inner;
    return tracked.get() * 2;
});
var detachedSeen = false;
var detached;
detached = new Signal.Computed(function() {
    detachedSeen = Signal.subtle.currentComputed() === detached;
    return hidden.get();
});
var outerStart = false;
var outerAfterInner = false;
var untrackNone = false;
var untrackAfterNested = false;
var outerAfterUntrack = false;
var outerAfterThrow = false;
var marker = {};
var outer;
outer = new Signal.Computed(function() {
    outerStart = Signal.subtle.currentComputed() === outer;
    var value = inner.get();
    outerAfterInner = Signal.subtle.currentComputed() === outer;
    Signal.subtle.untrack(function() {
        untrackNone = Signal.subtle.currentComputed() === undefined;
        detached.get();
        untrackAfterNested = Signal.subtle.currentComputed() === undefined;
    });
    outerAfterUntrack = Signal.subtle.currentComputed() === outer;
    try { Signal.subtle.untrack(function() { throw marker; }); } catch (error) {}
    outerAfterThrow = Signal.subtle.currentComputed() === outer;
    return value + tracked.get();
});
var nested = outer.get() === 3 && innerSeen && detachedSeen && outerStart && outerAfterInner &&
    untrackNone && untrackAfterNested && outerAfterUntrack && outerAfterThrow &&
    Signal.subtle.currentComputed() === undefined;

var equalsSource = new Signal.State(1);
var equalsComputeSeen = false;
var equalsSeen = false;
var equalComputed;
equalComputed = new Signal.Computed(function() {
    equalsComputeSeen = Signal.subtle.currentComputed() === equalComputed;
    return equalsSource.get();
}, { equals: function() {
    equalsSeen = Signal.subtle.currentComputed() === equalComputed;
    return false;
} });
equalComputed.get();
equalsSource.set(2);
var equalsOwner = equalComputed.get() === 2 && equalsComputeSeen && equalsSeen &&
    Signal.subtle.currentComputed() === undefined;

var hookOwner = {};
var hookState = new Signal.State(1, {
    [Signal.subtle.watched]: function() { hookOwner = Signal.subtle.currentComputed(); }
});
var hookComputed = new Signal.Computed(function() { return hookState.get(); });
var hookWatcher = new Signal.subtle.Watcher(function() {});
hookWatcher.watch(hookComputed);
hookComputed.get();
var hookNone = hookOwner === undefined;

var notifyOwner = {};
var notifyState = new Signal.State(0);
var notifyWatcher = new Signal.subtle.Watcher(function() {
    notifyOwner = Signal.subtle.currentComputed();
});
notifyWatcher.watch(notifyState);
notifyState.set(1);
var notifyNone = notifyOwner === undefined;

var throwMarker = {};
var throwSeen = false;
var throwing;
throwing = new Signal.Computed(function() {
    throwSeen = Signal.subtle.currentComputed() === throwing;
    throw throwMarker;
});
var throwIdentity = false;
try { throwing.get(); } catch (error) { throwIdentity = error === throwMarker; }
var throwRestored = throwIdentity && throwSeen && Signal.subtle.currentComputed() === undefined;

topLevel && nested && equalsOwner && hookNone && notifyNone && throwRestored;
"#;

const SIGNAL_UNTRACK_SOURCE: &str = r#"
var tracked = new Signal.State(1);
var hidden = new Signal.State(10);
var tail = new Signal.State(100);
var innerCalls = 0;
var inner = new Signal.Computed(function() { innerCalls++; return hidden.get() * 2; });
var outerCalls = 0;
var marker = {};
var caughtInside = 0;
var invalidCaught = 0;
var outer = new Signal.Computed(function() {
    outerCalls++;
    var first = tracked.get();
    var middle = Signal.subtle.untrack(function() {
        return Signal.subtle.untrack(function() { return inner.get(); });
    });
    try {
        Signal.subtle.untrack(function() { hidden.get(); throw marker; });
    } catch (error) {
        if (error === marker) caughtInside++;
    }
    try { Signal.subtle.untrack(1); } catch (error) {
        if (error instanceof TypeError) invalidCaught++;
    }
    return first + middle + tail.get();
});
var initial = outer.get() === 121 && outerCalls === 1 && innerCalls === 1 &&
    caughtInside === 1 && invalidCaught === 1;
hidden.set(11);
var hiddenIgnored = outer.get() === 121 && outerCalls === 1 && innerCalls === 1;
tail.set(101);
var tailTracked = outer.get() === 124 && outerCalls === 2 && innerCalls === 2 &&
    caughtInside === 2 && invalidCaught === 2;
tracked.set(2);
var ownerRestored = outer.get() === 125 && outerCalls === 3 && innerCalls === 2;

var token = {};
var nestedReturn = Signal.subtle.untrack(function() {
    return Signal.subtle.untrack(function() { return token; });
}) === token;
var topMarker = {};
var topIdentity = false;
try { Signal.subtle.untrack(function() { throw topMarker; }); }
catch (error) { topIdentity = error === topMarker; }
var proxyCalls = 0;
var proxyCallback = new Proxy(function() {}, { apply: function() { proxyCalls++; return token; } });
var proxyResult = Signal.subtle.untrack(proxyCallback) === token && proxyCalls === 1;

var frozenState = new Signal.State(0);
var frozenCalls = 0;
var frozenRejected = 0;
var frozenWatcher = new Signal.subtle.Watcher(function() {
    try {
        Signal.subtle.untrack(function() { frozenCalls++; frozenState.get(); });
    } catch (error) {
        if (error instanceof TypeError) frozenRejected++;
    }
});
frozenWatcher.watch(frozenState);
frozenState.set(1);
var frozen = frozenRejected === 1 && frozenCalls === 0;

initial && hiddenIgnored && tailTracked && ownerRestored && nestedReturn && topIdentity &&
proxyResult && frozen;
"#;

const SIGNAL_CROSS_REALM_SOURCE: &str = r#"
var identities = foreignSignal !== Signal && foreignSignal.State !== Signal.State &&
    foreignSignal.State.prototype !== Signal.State.prototype &&
    foreignSignal.Computed !== Signal.Computed &&
    foreignSignal.subtle.Watcher !== Signal.subtle.Watcher &&
    foreignSignal.subtle.untrack !== Signal.subtle.untrack &&
    foreignSignal.subtle.currentComputed !== Signal.subtle.currentComputed &&
    foreignSignal.subtle.introspectSources !== Signal.subtle.introspectSources &&
    foreignSignal.subtle.introspectSinks !== Signal.subtle.introspectSinks &&
    foreignSignal.subtle.hasSources !== Signal.subtle.hasSources &&
    foreignSignal.subtle.hasSinks !== Signal.subtle.hasSinks &&
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
var foreignUntrack = foreignSignal.subtle.untrack(function() { return local.get() + foreign.get(); }) === 5;
var foreignCurrent = false;
var localComputed;
localComputed = new Signal.Computed(function() {
    foreignCurrent = foreignSignal.subtle.currentComputed() === localComputed;
    return local.get();
});
var crossIntrospection = false;
localComputed.get();
var foreignSources = foreignSignal.subtle.introspectSources(localComputed);
var foreignComputedValue = computed.get() === 3;
var localSources = Signal.subtle.introspectSources(computed);
var crossWatcher = new foreignSignal.subtle.Watcher(function() {});
crossWatcher.watch(localComputed);
crossIntrospection = foreignSources.length === 1 && foreignSources[0] === local &&
    localSources.length === 1 && localSources[0] === foreign &&
    Signal.subtle.introspectSources(crossWatcher)[0] === localComputed &&
    foreignSignal.subtle.introspectSinks(localComputed)[0] === crossWatcher &&
    foreignSignal.subtle.hasSources(localComputed) && Signal.subtle.hasSources(crossWatcher) &&
    Signal.subtle.hasSinks(localComputed);
crossWatcher.unwatch(localComputed);
identities && crossBrand && subclassed && foreignComputedValue && callbackReceiver === computed &&
foreignUntrack && localComputed.get() === 2 && foreignCurrent && crossIntrospection;
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

const SIGNAL_WATCHER_STATE_SOURCE: &str = r#"
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

const SIGNAL_CHECKED_PULL_SOURCE: &str = r#"
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

const SIGNAL_COMPUTED_EQUALS_SOURCE: &str = r#"
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

const SIGNAL_COMPUTED_HOOKS_SOURCE: &str = r#"
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

const SIGNAL_GC_LIVENESS_SETUP_SOURCE: &str = r#"
var gcHookTrace = "";
var rootedSource = new Signal.State({ marker: 1 }, {
    [Signal.subtle.watched]: function() { gcHookTrace += "w"; },
    [Signal.subtle.unwatched]: function() { gcHookTrace += "u"; }
});
var transientWatcher = new Signal.subtle.Watcher(function() {});
transientWatcher.watch(rootedSource);
var weakWatcher = new WeakRef(transientWatcher);

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

const SIGNAL_GC_LIVENESS_DROP_ROOTS_SOURCE: &str = r#"
transientWatcher = null;
coldComputed = null;
cycleSource = null;
cycleComputed = null;
cycleWatcher = null;
true;
"#;

const SIGNAL_GC_LIVENESS_AFTER_FIRST_MAJOR_SOURCE: &str = r#"
var recoveredWatcher = weakWatcher.deref();
var activeWatcherRetained = recoveredWatcher !== undefined;
var coldComputedCollected = weakColdComputed.deref() === undefined;
var cycleSourceCollected = weakCycleSource.deref() === undefined;
var cycleComputedCollected = weakCycleComputed.deref() === undefined;
var cycleWatcherCollected = weakCycleWatcher.deref() === undefined;
var collectionSkippedHooks = gcHookTrace === "wc";
recoveredWatcher.unwatch(rootedSource);
recoveredWatcher = null;
activeWatcherRetained && coldComputedCollected && cycleSourceCollected &&
cycleComputedCollected && cycleWatcherCollected && collectionSkippedHooks && gcHookTrace === "wcu";
"#;

const SIGNAL_GC_LIVENESS_AFTER_UNWATCH_MAJOR_SOURCE: &str = r#"
weakWatcher.deref() === undefined && rootedSource.get().marker === 1 && gcHookTrace === "wcu";
"#;

const SIGNAL_GC_LIVENESS_DROP_RECOVERED_SOURCE: &str = r#"
recoveredWatcher = null;
true;
"#;

const SIGNAL_NOTIFY_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceNotifyState = new Signal.State(0);
var resourceNotifyCalls = 0;
var resourceNotifyWatcher = new Signal.subtle.Watcher(function() { resourceNotifyCalls++; });
resourceNotifyWatcher.watch(resourceNotifyState);
true;
"#;

const SIGNAL_NOTIFY_RESOURCE_FAIL_SOURCE: &str = "resourceNotifyState.set(1);";

const SIGNAL_NOTIFY_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceNotifyWatcher.watch();
resourceNotifyState.set(2);
resourceNotifyCalls === 1 && resourceNotifyState.get() === 2;
"#;

const SIGNAL_COMPUTED_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceComputedState = new Signal.State(1);
var resourceComputedCalls = 0;
var resourceComputed = new Signal.Computed(function() {
    resourceComputedCalls++;
    return resourceComputedState.get() + 1;
});
true;
"#;

const SIGNAL_COMPUTED_RESOURCE_FAIL_SOURCE: &str = "resourceComputed.get();";

const SIGNAL_COMPUTED_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceComputed.get() === 2 && resourceComputedCalls === 1 &&
Signal.subtle.currentComputed() === undefined;
"#;

const SIGNAL_EQUALS_RESOURCE_SETUP_SOURCE: &str = r#"
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

const SIGNAL_EQUALS_RESOURCE_DIRTY_SOURCE: &str = "resourceEqualsState.set(2); true;";
const SIGNAL_EQUALS_RESOURCE_FAIL_SOURCE: &str = "resourceEquals.get();";

const SIGNAL_EQUALS_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceEquals.get() === 2 && resourceEqualsComputations === 2 && resourceEqualsCalls === 1 &&
Signal.subtle.currentComputed() === undefined;
"#;

const SIGNAL_UNTRACK_RESOURCE_SETUP_SOURCE: &str = r#"
var resourceUntrackCalls = 0;
var resourceUntrackOwner = false;
var resourceUntrack = new Signal.Computed(function() {
    resourceUntrackCalls++;
    resourceUntrackOwner = Signal.subtle.currentComputed() === resourceUntrack;
    return Signal.subtle.untrack(function() { return 7; });
});
true;
"#;

const SIGNAL_UNTRACK_RESOURCE_FAIL_SOURCE: &str = "resourceUntrack.get();";

const SIGNAL_UNTRACK_RESOURCE_RECOVER_SOURCE: &str = r#"
resourceUntrack.get() === 7 && resourceUntrackCalls === 2 && resourceUntrackOwner &&
Signal.subtle.currentComputed() === undefined;
"#;

#[derive(Clone, Copy)]
struct SignalResourceCase {
    setup: &'static str,
    failure: &'static str,
    recovery: &'static str,
    dirty: Option<&'static str>,
    completion_limit: u32,
    expects_stack_limit: bool,
    source_id: u32,
    name: &'static str,
}

const PINNED_PROPOSAL_FIXTURES: &[(&str, &str)] = &[
    (
        "state-computed",
        include_str!("../../../../tests/proposal-signals/state-computed.js"),
    ),
    (
        "custom-equality-errors",
        include_str!("../../../../tests/proposal-signals/custom-equality-errors.js"),
    ),
    (
        "cycles-pruning",
        include_str!("../../../../tests/proposal-signals/cycles-pruning.js"),
    ),
    (
        "dynamic-graph",
        include_str!("../../../../tests/proposal-signals/dynamic-graph.js"),
    ),
    (
        "watcher-liveness",
        include_str!("../../../../tests/proposal-signals/watcher-liveness.js"),
    ),
    (
        "receivers-frozen",
        include_str!("../../../../tests/proposal-signals/receivers-frozen.js"),
    ),
    (
        "untrack-introspection-brands",
        include_str!("../../../../tests/proposal-signals/untrack-introspection-brands.js"),
    ),
];

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
fn assert_signal_job<const N: usize>(
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

/// Injects completion quota failures without allocator globals, then proves the isolate remains usable.
fn assert_signal_resource_restoration<const N: usize>(forced_major: bool) {
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
