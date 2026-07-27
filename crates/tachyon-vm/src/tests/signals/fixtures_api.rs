pub(super) const SIGNAL_SOURCE: &str = r#"
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

pub(super) const SIGNAL_API_CONTRACT_SOURCE: &str = r#"
function builtinDescriptor(object, key, writable, configurable) {
    var descriptor = Object.getOwnPropertyDescriptor(object, key);
    return descriptor !== undefined && descriptor.writable === writable &&
        descriptor.enumerable === false && descriptor.configurable === configurable;
}
var globalDescriptor = builtinDescriptor(this, "Signal", true, true);
var namespaceDescriptors = builtinDescriptor(Signal, "State", true, true) &&
    builtinDescriptor(Signal, "Computed", true, true) &&
    builtinDescriptor(Signal, "isState", true, true) &&
    builtinDescriptor(Signal, "isComputed", true, true) &&
    builtinDescriptor(Signal, "isWatcher", true, true) &&
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
    Signal.isState.name === "isState" && Signal.isState.length === 1 &&
    Signal.isComputed.name === "isComputed" && Signal.isComputed.length === 1 &&
    Signal.isWatcher.name === "isWatcher" && Signal.isWatcher.length === 1 &&
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
try { new Signal.isState({}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.isComputed({}); newOnly = false; } catch (error) {
    newOnly = newOnly && error instanceof TypeError;
}
try { new Signal.isWatcher({}); newOnly = false; } catch (error) {
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

pub(super) const SIGNAL_INTROSPECTION_SOURCE: &str = r#"
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

pub(super) const SIGNAL_CURRENT_COMPUTED_SOURCE: &str = r#"
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

pub(super) const SIGNAL_UNTRACK_SOURCE: &str = r#"
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

pub(super) const SIGNAL_COMPUTED_WRITE_SOURCE: &str = r#"
var state = new Signal.State(1);
var calls = 0;
var computed = new Signal.Computed(function() {
    calls++;
    state.set(state.get() + 1);
    return state.get();
});
var initial = computed.get() === 2 && state.get() === 2 && calls === 1;
var cached = computed.get() === 2 && state.get() === 2 && calls === 1;
state.set(3);
var updated = computed.get() === 4 && state.get() === 4 && calls === 2;
var recached = computed.get() === 4 && state.get() === 4 && calls === 2;

var outerState = new Signal.State(10);
var innerCalls = 0;
var inner = new Signal.Computed(function() {
    innerCalls++;
    outerState.set(outerState.get() + 1);
    return outerState.get();
});
var outerCalls = 0;
var outer = new Signal.Computed(function() {
    outerCalls++;
    return inner.get() * 2;
});
var nestedInitial = outer.get() === 22 && outerState.get() === 11 &&
    innerCalls === 1 && outerCalls === 1;
var nestedCached = outer.get() === 22 && innerCalls === 1 && outerCalls === 1;
outerState.set(20);
var nestedUpdated = outer.get() === 42 && outerState.get() === 21 &&
    innerCalls === 2 && outerCalls === 2;
initial && cached && updated && recached && nestedInitial && nestedCached && nestedUpdated;
"#;

pub(super) const SIGNAL_CALLABLE_PROXY_SOURCE: &str = r#"
var stateEqualsThis = false;
var stateEqualsArgs = false;
var stateEqualsCalls = 0;
var stateEquals = new Proxy(function(oldValue, newValue) {
    stateEqualsCalls++;
    stateEqualsThis = this === state;
    stateEqualsArgs = stateEqualsArgs || (oldValue === 1 && newValue === 2);
    return false;
}, {});
var state = new Signal.State(1, { equals: stateEquals });
state.set(2);

var callbackThis = false;
var callbackCalls = 0;
var callback = new Proxy(function() {
    callbackCalls++;
    callbackThis = this === computed;
    return state.get() * 2;
}, {});
var computedEqualsThis = false;
var computedEqualsArgs = false;
var computedEqualsCalls = 0;
var computedEquals = new Proxy(function(oldValue, newValue) {
    computedEqualsCalls++;
    computedEqualsThis = this === computed;
    computedEqualsArgs = oldValue === 4 && newValue === 6;
    return false;
}, {});
var computed = new Signal.Computed(callback, { equals: computedEquals });
var initial = computed.get() === 4 && callbackCalls === 1 && callbackThis;

var notifyThis = false;
var notifyCalls = 0;
var watcher;
var notify = new Proxy(function() {
    notifyCalls++;
    notifyThis = this === watcher;
}, {});
watcher = new Signal.subtle.Watcher(notify);
watcher.watch(computed);
state.set(3);
var notified = notifyCalls === 1 && notifyThis && watcher.getPending()[0] === computed;
var updated = computed.get() === 6 && callbackCalls === 2 && computedEqualsCalls === 1 &&
    computedEqualsThis && computedEqualsArgs && watcher.getPending().length === 0;

var nonCallableProxy = new Proxy({}, {});
var rejected = 0;
try { new Signal.Computed(nonCallableProxy); } catch (error) {
    if (error instanceof TypeError) rejected++;
}
try { new Signal.Computed(function() {}, { equals: nonCallableProxy }); } catch (error) {
    if (error instanceof TypeError) rejected++;
}
try { new Signal.subtle.Watcher(nonCallableProxy); } catch (error) {
    if (error instanceof TypeError) rejected++;
}
initial && notified && updated && stateEqualsCalls === 2 && stateEqualsThis && stateEqualsArgs &&
rejected === 3;
"#;

pub(super) const SIGNAL_CROSS_REALM_SOURCE: &str = r#"
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
    foreignSignal.isState !== Signal.isState &&
    foreignSignal.isComputed !== Signal.isComputed &&
    foreignSignal.isWatcher !== Signal.isWatcher &&
    foreignSignal.subtle.watched !== Signal.subtle.watched &&
    foreignSignal.subtle.unwatched !== Signal.subtle.unwatched;
var local = new Signal.State(2);
var foreign = new foreignSignal.State(3);
var crossBrand = Signal.State.prototype.get.call(foreign) === 3 &&
    foreignSignal.State.prototype.get.call(local) === 2 &&
    Signal.isState(foreign) && foreignSignal.isState(local) &&
    !Signal.isComputed(foreign) && !foreignSignal.isWatcher(local);
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
var stateTrace = "";
var stateHookThis = false;
var stateOptions = {};
Object.defineProperty(stateOptions, "equals", { get: function() {
    stateTrace += "e";
    return undefined;
} });
Object.defineProperty(stateOptions, Signal.subtle.watched, { get: function() {
    stateTrace += "x";
    throw 101;
} });
Object.defineProperty(stateOptions, foreignSignal.subtle.watched, { get: function() {
    stateTrace += "w";
    return function() { stateTrace += "W"; stateHookThis = this === foreignHookedState; };
} });
Object.defineProperty(stateOptions, foreignSignal.subtle.unwatched, { get: function() {
    stateTrace += "u";
    return function() { stateTrace += "U"; stateHookThis = stateHookThis && this === foreignHookedState; };
} });
var foreignHookedState = new ForeignStateSubclass(5, stateOptions);
var localWatcher = new Signal.subtle.Watcher(function() {});
localWatcher.watch(foreignHookedState);
localWatcher.unwatch(foreignHookedState);
var foreignStateOptions = stateTrace === "ewuWU" && stateHookThis;

var computedTrace = "";
var computedHookThis = false;
var computedOptions = {};
Object.defineProperty(computedOptions, "equals", { get: function() {
    computedTrace += "e";
    return undefined;
} });
Object.defineProperty(computedOptions, Signal.subtle.watched, { get: function() {
    computedTrace += "x";
    throw 103;
} });
Object.defineProperty(computedOptions, foreignSignal.subtle.watched, { get: function() {
    computedTrace += "w";
    return function() { computedTrace += "W"; computedHookThis = this === foreignHookedComputed; };
} });
Object.defineProperty(computedOptions, foreignSignal.subtle.unwatched, { get: function() {
    computedTrace += "u";
    return function() {
        computedTrace += "U";
        computedHookThis = computedHookThis && this === foreignHookedComputed;
    };
} });
var hookSource = new Signal.State(6);
var foreignHookedComputed = new ForeignComputedSubclass(function() {
    return hookSource.get();
}, computedOptions);
localWatcher.watch(foreignHookedComputed);
foreignHookedComputed.get();
localWatcher.unwatch(foreignHookedComputed);
var foreignComputedOptions = computedTrace === "ewuWU" && computedHookThis;

var cycleGate = new Signal.State(true);
var foreignCycle;
foreignCycle = new ForeignComputedSubclass(function() {
    return cycleGate.get() ? foreignCycle.get() : 42;
});
var cycleFirst;
var cycleCached;
try { foreignCycle.get(); } catch (error) { cycleFirst = error; }
try { foreignCycle.get(); } catch (error) { cycleCached = error; }
cycleGate.set(false);
var cycleRecovery = cycleFirst instanceof TypeError && cycleCached === cycleFirst &&
    foreignCycle.get() === 42;

class ForeignWatcherSubclass extends foreignSignal.subtle.Watcher {}
var notifyMarker = {};
var notifyState = new Signal.State(0);
var foreignSubclassWatcher = new ForeignWatcherSubclass(function() { throw notifyMarker; });
foreignSubclassWatcher.watch(notifyState);
var notifyIdentity = false;
try { notifyState.set(1); } catch (error) { notifyIdentity = error === notifyMarker; }
var watcherSubclass = Object.getPrototypeOf(foreignSubclassWatcher) ===
    ForeignWatcherSubclass.prototype && notifyIdentity;
identities && crossBrand && subclassed && foreignComputedValue && callbackReceiver === computed &&
foreignUntrack && localComputed.get() === 2 && foreignCurrent && crossIntrospection &&
foreignStateOptions && foreignComputedOptions && cycleRecovery && watcherSubclass;
"#;
