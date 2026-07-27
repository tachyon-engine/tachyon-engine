var __tachyonSignalsAssertionCount = 0;
function assert(value, message) {
    __tachyonSignalsAssertionCount++;
    if (!value) throw new Error(message);
}

var computedReceiver;
var computed;
computed = new Signal.Computed(function() { computedReceiver = this; });
computed.get();
assert(computedReceiver === computed, "Computed callback receiver");

var equalsReceiver;
var options = {
    equals: function() { equalsReceiver = this; return false; }
};
var state = new Signal.State(1, options);
state.set(2);
assert(equalsReceiver === state, "State equals receiver");
var equalComputed = new Signal.Computed(function() { return state.get(); }, options);
equalComputed.get();
state.set(3);
equalComputed.get();
assert(equalsReceiver === equalComputed, "Computed equals receiver");

var watchedReceiver;
var unwatchedReceiver;
var hooked = new Signal.State(1, {
    [Signal.subtle.watched]: function() { watchedReceiver = this; },
    [Signal.subtle.unwatched]: function() { unwatchedReceiver = this; }
});
var hookWatcher = new Signal.subtle.Watcher(function() {});
hookWatcher.watch(hooked);
hookWatcher.unwatch(hooked);
assert(watchedReceiver === hooked && unwatchedReceiver === hooked, "lifecycle hook receivers");

var frozenState = new Signal.State(1);
var frozenReads = 0;
var frozenWrites = 0;
var watcher;
watcher = new Signal.subtle.Watcher(function() {
    assert(this === watcher, "Watcher notify receiver");
    try { frozenState.get(); } catch (error) { if (error instanceof TypeError) frozenReads++; }
    try { frozenState.set(4); } catch (error) { if (error instanceof TypeError) frozenWrites++; }
});
watcher.watch(frozenState);
frozenState.set(2);
assert(frozenReads === 1 && frozenWrites === 1, "notify freezes Signal reads and writes");
watcher.unwatch(frozenState);
frozenState.set(3);
assert(frozenState.get() === 3, "graph unfreezes after notify");
true;
