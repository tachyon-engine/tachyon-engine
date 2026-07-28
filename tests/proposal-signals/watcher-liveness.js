var __tachyonSignalsAssertionCount = 0;
function assert(value, message) {
    __tachyonSignalsAssertionCount++;
    if (!value) throw new Error(message);
}

var watched = 0;
var unwatched = 0;
var state = new Signal.State(1, {
    [Signal.subtle.watched]: function() { watched++; },
    [Signal.subtle.unwatched]: function() { unwatched++; }
});
var computed = new Signal.Computed(function() { return state.get(); });
computed.get();
var first = new Signal.subtle.Watcher(function() {});
var second = new Signal.subtle.Watcher(function() {});
first.watch(computed);
second.watch(computed);
assert(watched === 1 && unwatched === 0, "first descendant starts liveness once");
second.unwatch(computed);
assert(watched === 1 && unwatched === 0, "intermediate descendant preserves liveness");
first.unwatch(computed);
assert(watched === 1 && unwatched === 1, "last descendant ends liveness once");

var left = new Signal.State(1);
var right = new Signal.State(2);
var notifyCount = 0;
var watcher = new Signal.subtle.Watcher(function() { notifyCount++; });
watcher.watch(left, right);
left.set(4);
assert(notifyCount === 1 && watcher.getPending().length === 0, "State notify is synchronous");
right.set(8);
assert(notifyCount === 1, "Watcher waits until rearmed");
watcher.watch();
right.set(9);
assert(notifyCount === 2, "zero argument watch rearms");
watcher.unwatch(left);
watcher.watch();
left.set(5);
assert(notifyCount === 2, "unwatch removes membership");
right.set(10);
assert(notifyCount === 3, "remaining membership stays live");

var absent = new Signal.State(0);
var beforeNoop = Signal.subtle.introspectSources(watcher);
assert(watcher.unwatch(absent) === undefined, "unwatching an absent Signal is a no-op");
var afterNoop = Signal.subtle.introspectSources(watcher);
assert(afterNoop.length === beforeNoop.length && afterNoop[0] === beforeNoop[0],
       "absent unwatch preserves ordered membership");
absent.set(1);
assert(notifyCount === 3, "absent unwatch does not attach the Signal");

var dirtySource = new Signal.State(1);
var dirty = new Signal.Computed(function() { return dirtySource.get(); });
var dirtyWatcher = new Signal.subtle.Watcher(function() {});
dirtyWatcher.watch(dirty);
assert(dirtyWatcher.getPending()[0] === dirty, "uninitialized Computed is pending");
dirty.get();
assert(dirtyWatcher.getPending().length === 0, "get clears pending Computed");
true;
