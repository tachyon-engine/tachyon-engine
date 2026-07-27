function assert(value, message) {
    if (!value) throw new Error(message);
}

var selector = new Signal.State(true);
var left = new Signal.State("left");
var right = new Signal.State("right");
var selectedCalls = 0;
var selected = new Signal.Computed(function() {
    selectedCalls++;
    return selector.get() ? left.get() : right.get();
});
assert(selected.get() === "left" && selectedCalls === 1, "dynamic graph initial branch");
right.set("unused");
assert(selected.get() === "left" && selectedCalls === 1, "inactive dependency ignored");
selector.set(false);
assert(selected.get() === "unused" && selectedCalls === 2, "dynamic graph switches branch");
left.set("stale");
assert(selected.get() === "unused" && selectedCalls === 2, "obsolete dependency removed");
right.set("active");
assert(selected.get() === "active" && selectedCalls === 3, "new dependency active");

var diamondSource = new Signal.State("a");
var diamondLeft = new Signal.Computed(function() { return diamondSource.get(); });
var diamondRight = new Signal.Computed(function() { return diamondSource.get(); });
var diamondCalls = 0;
var diamond = new Signal.Computed(function() {
    diamondCalls++;
    return diamondLeft.get() + " " + diamondRight.get();
});
assert(diamond.get() === "a a" && diamondCalls === 1, "diamond initial");
diamondSource.set("b");
assert(diamond.get() === "b b" && diamondCalls === 2, "diamond recomputes once");

var order = "";
var topoSource = new Signal.State(false);
var topoLeft = new Signal.Computed(function() { topoSource.get(); order += "l"; },
    { equals: function() { return false; } });
var topoRight = new Signal.Computed(function() { topoSource.get(); order += "r"; },
    { equals: function() { return false; } });
var topo = new Signal.Computed(function() { topoLeft.get(); topoRight.get(); order += "t"; },
    { equals: function() { return false; } });
topo.get();
order = "";
topoSource.set(true);
topo.get();
assert(order === "lrt", "topological order");
true;
