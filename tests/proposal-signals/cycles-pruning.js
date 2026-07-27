function assert(value, message) {
    if (!value) throw new Error(message);
}

var cycle;
cycle = new Signal.Computed(function() { return cycle.get(); });
var cycleFirst;
var cycleCached;
try { cycle.get(); } catch (error) { cycleFirst = error; }
try { cycle.get(); } catch (error) { cycleCached = error; }
assert(cycleFirst instanceof TypeError && cycleCached === cycleFirst,
    "trivial cycle is detected and cached");
var first;
var second;
first = new Signal.Computed(function() { return second.get(); });
second = new Signal.Computed(function() { return first.get(); });
var tail = new Signal.Computed(function() { return second.get(); });
var largerCycle;
try { tail.get(); } catch (error) { largerCycle = error; }
assert(largerCycle !== undefined, "larger cycle is detected");

var source = new Signal.State(0);
var stableCalls = 0;
var stable = new Signal.Computed(function() {
    stableCalls++;
    source.get();
    return 5;
});
var middleCalls = 0;
var middle = new Signal.Computed(function() {
    middleCalls++;
    return stable.get() + 1;
});
var topCalls = 0;
var top = new Signal.Computed(function() {
    topCalls++;
    return middle.get() + 1;
});
assert(top.get() === 7 && stableCalls === 1 && middleCalls === 1 && topCalls === 1,
    "pruning initial");
source.set(1);
assert(top.get() === 7 && stableCalls === 2 && middleCalls === 1 && topCalls === 1,
    "equal middle result prunes tail");

if (false) {
    var watcher = new Signal.subtle.Watcher(function() {});
    watcher.watch(top);
    source.set(2);
    assert(watcher.getPending().length === 1, "live pruned tail starts pending");
    top.get();
    assert(watcher.getPending().length === 0 && topCalls === 1, "live pruning clears pending");
}
true;
