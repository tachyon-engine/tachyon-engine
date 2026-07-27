function assert(value, message) {
    if (!value) throw new Error(message);
}

var state = new Signal.State(0);
assert(state.get() === 0, "State initial value");
state.set(10);
assert(state.get() === 10, "State updated value");

var nanState = new Signal.State(NaN);
var nanCalls = 0;
var nanComputed = new Signal.Computed(function() {
    nanCalls++;
    return nanState.get();
});
assert(nanCalls === 0, "Computed is lazy");
assert(Number.isNaN(nanComputed.get()) && nanCalls === 1, "Computed evaluates once");
nanState.set(NaN);
assert(Number.isNaN(nanComputed.get()) && nanCalls === 1, "State defaults to Object.is");

var source = new Signal.State(1);
var value = 5;
var inner = new Signal.Computed(function() {
    source.get();
    return value;
});
var outerCalls = 0;
var outer = new Signal.Computed(function() {
    outerCalls++;
    return inner.get();
});
assert(outer.get() === 5 && outerCalls === 1, "Computed chain initial value");
source.set(2);
assert(outer.get() === 5 && outerCalls === 1, "unchanged Computed prunes downstream");
value = NaN;
source.set(3);
assert(Number.isNaN(outer.get()) && outerCalls === 2, "changed Computed updates downstream");
source.set(4);
assert(Number.isNaN(outer.get()) && outerCalls === 2, "Computed Object.is handles NaN");
true;
