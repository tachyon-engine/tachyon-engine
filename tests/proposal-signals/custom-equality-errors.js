var __tachyonSignalsAssertionCount = 0;
function assert(value, message) {
    __tachyonSignalsAssertionCount++;
    if (!value) throw new Error(message);
}

var answer = true;
var state = new Signal.State(1, {
    equals: function() { return answer; }
});
var stateCalls = 0;
var stateComputed = new Signal.Computed(function() {
    stateCalls++;
    return state.get();
});
assert(stateComputed.get() === 1 && stateCalls === 1, "custom State equality initial");
state.set(2);
assert(state.get() === 1 && stateComputed.get() === 1 && stateCalls === 1,
    "custom State equality suppresses update");
answer = false;
state.set(2);
assert(state.get() === 2 && stateComputed.get() === 2 && stateCalls === 2,
    "custom State equality permits update");

var errorSource = new Signal.State("first");
var errorCalls = 0;
var throwing = new Signal.Computed(function() {
    errorCalls++;
    throw errorSource.get();
});
var first;
var cached;
try { throwing.get(); } catch (error) { first = error; }
try { throwing.get(); } catch (error) { cached = error; }
assert(first === "first" && cached === "first" && errorCalls === 1,
    "Computed caches abrupt completion");
errorSource.set("second");
var second;
try { throwing.get(); } catch (error) { second = error; }
assert(second === "second" && errorCalls === 2, "dependency invalidates cached error");

var equalsSource = new Signal.State(0);
var equalsCalls = 0;
var equalsMarker = {};
var equalsThrow = new Signal.Computed(function() { return equalsSource.get(); }, {
    equals: function() { equalsCalls++; throw equalsMarker; }
});
equalsThrow.get();
equalsSource.set(1);
var equalsFirst;
var equalsCached;
try { equalsThrow.get(); } catch (error) { equalsFirst = error; }
try { equalsThrow.get(); } catch (error) { equalsCached = error; }
assert(equalsFirst === equalsMarker && equalsCached === equalsMarker && equalsCalls === 1,
    "Computed caches equality error identity");
true;
