function assert(value, message) {
    if (!value) throw new Error(message);
}

var watched1 = 0;
var unwatched1 = 0;
var watched2 = 0;
var unwatched2 = 0;
var notifications = 0;
var derives = 0;
var source1 = new Signal.State(1, {
    [Signal.subtle.watched]: function() { watched1++; },
    [Signal.subtle.unwatched]: function() { unwatched1++; }
});
var source2 = new Signal.State(2, {
    [Signal.subtle.watched]: function() { watched2++; },
    [Signal.subtle.unwatched]: function() { unwatched2++; }
});
var selected = source1;
var derived = new Signal.Computed(function() {
    derives++;
    return selected.get();
});
var watcher = new Signal.subtle.Watcher(function() { notifications++; });

watcher.watch(derived);
assert(watcher.getPending().length === 1, "new watcher starts pending");
assert(derived.get() === 1, "initial source value");
assert(watched1 === 1 && unwatched1 === 0, "first source becomes live");
assert(!Signal.subtle.hasSinks(source2), "second source remains cold");

source1.set(3);
assert(notifications === 1 && watcher.getPending()[0] === derived, "first invalidation");
assert(derived.get() === 3 && derives === 2, "first source refresh");

selected = source2;
watcher.watch();
source1.set(4);
assert(notifications === 2, "rearmed watcher invalidates");
assert(derived.get() === 2, "dependency switches to second source");
assert(watched1 === 1 && unwatched1 === 1, "first source becomes cold");
assert(watched2 === 1 && unwatched2 === 0, "second source becomes live");

selected = { get: function() { return 10; } };
watcher.watch();
source1.set(5);
assert(derived.get() === 2 && derives === 3, "detached source cannot invalidate");
source2.set(0);
assert(notifications === 3 && watcher.getPending()[0] === derived, "second invalidation");
assert(derived.get() === 10 && derives === 4, "dependency switches to plain getter");
assert(watched2 === 1 && unwatched2 === 1, "second source becomes cold");
assert(!Signal.subtle.hasSinks(source1) && !Signal.subtle.hasSinks(source2), "all sources detach");
true;
