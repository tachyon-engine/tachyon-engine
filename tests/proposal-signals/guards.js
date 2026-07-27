var __tachyonSignalsAssertionCount = 0;
function assert(value, message) {
    __tachyonSignalsAssertionCount++;
    if (!value) throw new Error(message);
}

var state = new Signal.State(1);
var computed = new Signal.Computed(function() { return state.get(); });
var watcher = new Signal.subtle.Watcher(function() {});

assert(Signal.isState(state), "State guard accepts State");
assert(!Signal.isState(computed) && !Signal.isState(watcher),
    "State guard rejects other Signal brands");
assert(Signal.isComputed(computed), "Computed guard accepts Computed");
assert(!Signal.isComputed(state) && !Signal.isComputed(watcher),
    "Computed guard rejects other Signal brands");
assert(Signal.isWatcher(watcher), "Watcher guard accepts Watcher");
assert(!Signal.isWatcher(state) && !Signal.isWatcher(computed),
    "Watcher guard rejects other Signal brands");

class StateSubclass extends Signal.State {}
class ComputedSubclass extends Signal.Computed {}
class WatcherSubclass extends Signal.subtle.Watcher {}
assert(Signal.isState(new StateSubclass(2)), "State guard accepts subclasses");
assert(Signal.isComputed(new ComputedSubclass(function() { return 2; })),
    "Computed guard accepts subclasses");
assert(Signal.isWatcher(new WatcherSubclass(function() {})),
    "Watcher guard accepts subclasses");

var values = [undefined, null, true, 1, "state", {}, function() {}];
for (var index = 0; index < values.length; index++) {
    assert(!Signal.isState(values[index]) && !Signal.isComputed(values[index]) &&
        !Signal.isWatcher(values[index]), "guards return false for arbitrary values");
}

assert(!Signal.isState(new Proxy(state, {})) &&
    !Signal.isComputed(new Proxy(computed, {})) &&
    !Signal.isWatcher(new Proxy(watcher, {})), "guards do not unwrap Proxy brands");
true;
