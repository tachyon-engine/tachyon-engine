var __tachyonSignalsAssertionCount = 0;
function assert(value, message) {
    __tachyonSignalsAssertionCount++;
    if (!value) throw new Error(message);
}

// Solid: a jagged diamond with two tails recomputes each node once per change.
var jaggedSource = new Signal.State("a");
var jaggedLeft = new Signal.Computed(function() { return jaggedSource.get(); });
var jaggedRight = new Signal.Computed(function() { return jaggedSource.get(); });
var jaggedRightTail = new Signal.Computed(function() { return jaggedRight.get(); });
var jaggedJoinCalls = 0;
var jaggedJoin = new Signal.Computed(function() {
    jaggedJoinCalls++;
    return jaggedLeft.get() + jaggedRightTail.get();
});
var jaggedFirstCalls = 0;
var jaggedFirst = new Signal.Computed(function() {
    jaggedFirstCalls++;
    return jaggedJoin.get();
});
var jaggedSecondCalls = 0;
var jaggedSecond = new Signal.Computed(function() {
    jaggedSecondCalls++;
    return jaggedJoin.get();
});
assert(jaggedFirst.get() === "aa" && jaggedSecond.get() === "aa", "jagged initial");
jaggedSource.set("b");
assert(jaggedFirst.get() === "bb" && jaggedSecond.get() === "bb", "jagged changed");
assert(jaggedJoinCalls === 2 && jaggedFirstCalls === 2 && jaggedSecondCalls === 2,
    "jagged computes once");

// Solid: one static arm must not suppress a changed sibling.
var mixedSource = new Signal.State("a");
var mixedChanged = new Signal.Computed(function() { return mixedSource.get(); });
var mixedStatic = new Signal.Computed(function() {
    mixedSource.get();
    return "static";
});
var mixedCalls = 0;
var mixedJoin = new Signal.Computed(function() {
    mixedCalls++;
    return mixedChanged.get() + mixedStatic.get();
});
assert(mixedJoin.get() === "astatic", "mixed initial");
mixedSource.set("b");
assert(mixedJoin.get() === "bstatic" && mixedCalls === 2, "mixed changed sibling");

// Solid: linear and exponential convergence must not duplicate the tail pull.
var linearSource = new Signal.State(0);
var linearA = new Signal.Computed(function() { return linearSource.get(); });
var linearB = new Signal.Computed(function() { return linearSource.get(); });
var linearC = new Signal.Computed(function() { return linearSource.get(); });
var linearD = new Signal.Computed(function() { return linearSource.get(); });
var linearE = new Signal.Computed(function() { return linearSource.get(); });
var linearCalls = 0;
var linearTail = new Signal.Computed(function() {
    linearCalls++;
    return linearA.get() + linearB.get() + linearC.get() + linearD.get() + linearE.get();
});
assert(linearTail.get() === 0 && linearCalls === 1, "linear initial");
linearSource.set(1);
assert(linearTail.get() === 5 && linearCalls === 2, "linear convergence once");

var exponentialSource = new Signal.State(0);
var exponentialA = new Signal.Computed(function() { return exponentialSource.get(); });
var exponentialB = new Signal.Computed(function() { return exponentialSource.get(); });
var exponentialC = new Signal.Computed(function() { return exponentialSource.get(); });
var exponentialD = new Signal.Computed(function() {
    return exponentialA.get() + exponentialB.get() + exponentialC.get();
});
var exponentialE = new Signal.Computed(function() {
    return exponentialA.get() + exponentialB.get() + exponentialC.get();
});
var exponentialF = new Signal.Computed(function() {
    return exponentialA.get() + exponentialB.get() + exponentialC.get();
});
var exponentialCalls = 0;
var exponentialTail = new Signal.Computed(function() {
    exponentialCalls++;
    return exponentialD.get() + exponentialE.get() + exponentialF.get();
});
assert(exponentialTail.get() === 0 && exponentialCalls === 1, "exponential initial");
exponentialSource.set(1);
assert(exponentialTail.get() === 9 && exponentialCalls === 2, "exponential convergence once");

// Solid: changed dependees follow construction order after their trackers refresh.
var orderSource = new Signal.State(0);
var order = "";
var orderTracker = new Signal.Computed(function() {
    order += "t1";
    return orderSource.get() === 0;
});
var orderDirect = new Signal.Computed(function() {
    order += "c1";
    return orderSource.get();
});
var orderDependee = new Signal.Computed(function() {
    order += "c2";
    return orderTracker.get();
});
orderDirect.get();
orderDependee.get();
assert(order === "c1c2t1", "construction order initial");
order = "";
orderSource.set(1);
orderDirect.get();
orderDependee.get();
assert(order === "c1t1c2", "construction order changed");

// Solid: stale downstream computations remain pending after an unrelated pull.
var staleFirst = new Signal.State(1);
var staleSecond = new Signal.State(false);
var staleCount = 0;
var staleTrackerA = new Signal.Computed(function() { return staleFirst.get() > 0; });
var staleTrackerB = new Signal.Computed(function() { return staleFirst.get() > 0; });
var staleDirect = new Signal.Computed(function() { return staleFirst.get(); });
var staleTrackerC = new Signal.Computed(function() {
    return staleFirst.get() && staleSecond.get();
});
var staleTail = new Signal.Computed(function() {
    staleTrackerA.get();
    staleTrackerB.get();
    staleDirect.get();
    staleTrackerC.get();
    staleCount++;
});
staleTail.get();
staleSecond.set(true);
staleTail.get();
assert(staleCount === 2, "unrelated change refreshes tail");
staleFirst.set(2);
staleTail.get();
assert(staleCount === 3, "subsequent pending refresh survives");

// Solid: a deep changed chain is evaluated source-to-tail exactly once.
var deepSource = new Signal.State(1);
var deepOrder = "";
var deepOne = new Signal.Computed(function() {
    deepOrder += "one";
    return deepSource.get();
});
var deepTwo = new Signal.Computed(function() {
    deepOrder += "two";
    return deepOne.get();
});
var deepThree = new Signal.Computed(function() {
    deepOrder += "three";
    return deepTwo.get();
});
var deepFour = new Signal.Computed(function() {
    deepOrder += "four";
    return deepThree.get();
});
deepFour.get();
deepOrder = "";
deepSource.set(2);
deepFour.get();
assert(deepOrder === "onetwothreefour", "deep source-to-tail order");
true;
