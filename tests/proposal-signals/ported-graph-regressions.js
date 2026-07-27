var __tachyonSignalsAssertionCount = 0;
function assert(value, message) {
    __tachyonSignalsAssertionCount++;
    if (!value) throw new Error(message);
}

// Preact: a caught dependency error must not poison its dependee.
var errorSource = new Signal.State(0);
var dependencyError = new Error("dependency");
var throwing = new Signal.Computed(function() {
    errorSource.get();
    throw dependencyError;
});
var caughtCalls = 0;
var catches = new Signal.Computed(function() {
    caughtCalls++;
    try {
        throwing.get();
    } catch (error) {
        assert(error === dependencyError, "dependency error identity");
        return "ok";
    }
});
assert(catches.get() === "ok" && caughtCalls === 1, "caught dependency error initial");
errorSource.set(1);
assert(catches.get() === "ok" && caughtCalls === 2, "caught dependency error refresh");

// Preact: a flag-shaped graph must not lose the direct X -> B invalidation.
var flagSource = new Signal.State(2);
var flagA = new Signal.Computed(function() { return flagSource.get() - 1; });
var flagB = new Signal.Computed(function() { return flagSource.get() + flagA.get(); });
var flagCalls = 0;
var flagTail = new Signal.Computed(function() {
    flagCalls++;
    return flagB.get();
});
assert(flagTail.get() === 3 && flagCalls === 1, "flag graph initial");
flagSource.set(4);
assert(flagTail.get() === 7 && flagCalls === 2, "flag graph refresh");

// Solid: both unchanged diamond arms must prune the downstream computation.
var staticSource = new Signal.State("a");
var staticLeft = new Signal.Computed(function() {
    staticSource.get();
    return "left";
});
var staticRight = new Signal.Computed(function() {
    staticSource.get();
    return "right";
});
var staticTailCalls = 0;
var staticTail = new Signal.Computed(function() {
    staticTailCalls++;
    return staticLeft.get() + staticRight.get();
});
assert(staticTail.get() === "leftright" && staticTailCalls === 1, "static diamond initial");
staticSource.set("b");
assert(staticTail.get() === "leftright" && staticTailCalls === 1, "static diamond pruned");

// Solid: stale trackers run before dependees, including the mixed changed/unchanged case.
var orderedSource = new Signal.State(1, { equals: function() { return false; } });
var order = "";
var orderedFirst = new Signal.Computed(function() {
    order += "t1";
    return orderedSource.get() > 2;
});
var orderedSecond = new Signal.Computed(function() {
    order += "t2";
    return orderedSource.get() > 2;
});
var orderedAlways = new Signal.Computed(function() {
    order += "c1";
    orderedSource.get();
}, { equals: function() { return false; } });
var orderedTail = new Signal.Computed(function() {
    order += "c2";
    orderedFirst.get();
    orderedSecond.get();
    orderedAlways.get();
});
orderedTail.get();
order = "";
orderedSource.set(1);
orderedTail.get();
assert(order === "t1t2c1c2", "unchanged tracker order");
order = "";
orderedSource.set(3);
orderedTail.get();
assert(order === "t1c2t2c1", "changed tracker order");

// Vue: writes performed by a dependency during a pull do not stale the active result.
var sideEffectState = new Signal.State(0);
var sideEffectInner = new Signal.Computed(function() {
    if (sideEffectState.get() === 0) sideEffectState.set(1);
    return "value";
});
var sideEffectOuter = new Signal.Computed(function() {
    return sideEffectState.get() + sideEffectInner.get();
});
assert(sideEffectOuter.get() === "0value", "side effect pull result");
assert(sideEffectOuter.get() === "0value", "side effect result remains cached");

// Solid: a computation may return another lazy computation without losing ordering.
var nestedSource = new Signal.State(0);
var nestedOther = new Signal.State(0);
var nestedOrder = "";
var nestedTracker = new Signal.Computed(function() {
    nestedOrder += "tracker";
    return nestedSource.get() === 0;
});
var nestedDirect = new Signal.Computed(function() {
    nestedOrder += "direct";
    return nestedSource.get();
});
var nestedFactory = new Signal.Computed(function() {
    nestedOrder += "factory";
    nestedTracker.get();
    return new Signal.Computed(function() {
        nestedOrder += "inner";
        return nestedOther.get();
    });
});
nestedSource.set(1);
nestedDirect.get();
nestedFactory.get().get();
assert(nestedOrder === "directfactorytrackerinner", "nested computed ordering");
true;
