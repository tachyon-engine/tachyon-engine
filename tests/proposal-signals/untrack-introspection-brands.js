var __tachyonSignalsAssertionCount = 0;
function assert(value, message) {
    __tachyonSignalsAssertionCount++;
    if (!value) throw new Error(message);
}

assert(Signal.subtle.currentComputed() === undefined, "no top-level current Computed");
var state = new Signal.State(1);
var context;
var tracked;
tracked = new Signal.Computed(function() {
    context = Signal.subtle.currentComputed();
    return state.get();
});
tracked.get();
assert(context === tracked, "currentComputed identifies callback owner");

var hidden = new Signal.State(10);
var untrackedCalls = 0;
var untracked = new Signal.Computed(function() {
    untrackedCalls++;
    return Signal.subtle.untrack(function() { return hidden.get(); });
});
assert(untracked.get() === 10 && untrackedCalls === 1, "untrack initial value");
hidden.set(20);
assert(untracked.get() === 10 && untrackedCalls === 1, "untrack hides dependency");

var watcher = new Signal.subtle.Watcher(function() {});
watcher.watch(tracked);
var sources = Signal.subtle.introspectSources(tracked);
var sinks = Signal.subtle.introspectSinks(state);
assert(sources.length === 1 && sources[0] === state, "Computed source introspection");
assert(sinks.length === 1 && sinks[0] === tracked, "State sink introspection");
assert(Signal.subtle.hasSources(tracked) && Signal.subtle.hasSinks(state), "graph predicates");
sources[0] = hidden;
assert(Signal.subtle.introspectSources(tracked)[0] === state, "introspection is a fresh snapshot");

var wrong = {};
var brandErrors = 0;
try { Signal.State.prototype.get.call(wrong); } catch (error) {
    if (error instanceof TypeError) brandErrors++;
}
try { Signal.Computed.prototype.get.call(state); } catch (error) {
    if (error instanceof TypeError) brandErrors++;
}
try { Signal.subtle.Watcher.prototype.watch.call(tracked, state); } catch (error) {
    if (error instanceof TypeError) brandErrors++;
}
try { Signal.subtle.introspectSources(state); } catch (error) {
    if (error instanceof TypeError) brandErrors++;
}
try { Signal.subtle.introspectSinks(watcher); } catch (error) {
    if (error instanceof TypeError) brandErrors++;
}
assert(brandErrors === 5, "Signal method and introspection brands");
true;
