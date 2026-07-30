use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

const ITERATOR_FLAT_MAP_SOURCE: &str = r#"
var descriptor = Object.getOwnPropertyDescriptor(Iterator.prototype, "flatMap");
var descriptorOk = typeof descriptor.value === "function" &&
  descriptor.value.name === "flatMap" && descriptor.value.length === 1 &&
  descriptor.writable === true && descriptor.enumerable === false &&
  descriptor.configurable === true;

var directCursor = 0;
var direct = {
  next: function() {
    return directCursor < 2 ? { value: ++directCursor, done: false } : { done: true };
  }
};
var iterableCalls = 0;
var iterable = {
  [Symbol.iterator]: function() {
    iterableCalls++;
    return [3, 4].values();
  }
};
var outerCursor = 0;
var outer = Object.create(Iterator.prototype);
outer.next = function() {
  return outerCursor < 2 ? { value: ++outerCursor, done: false } : { done: true };
};
var mixed = outer.flatMap(function(value) { return value === 1 ? direct : iterable; });
var mixedValues = [mixed.next().value, mixed.next().value, mixed.next().value, mixed.next().value];
var mixedDone = mixed.next();
var flattenOk = mixedValues[0] === 1 && mixedValues[1] === 2 &&
  mixedValues[2] === 3 && mixedValues[3] === 4 && iterableCalls === 1 && mixedDone.done === true;

var depthOuter = [1].values().flatMap(function(value) { return [value, [value + 1]]; });
var depthFirst = depthOuter.next();
var depthSecond = depthOuter.next();
var depthOk = depthFirst.value === 1 && Array.isArray(depthSecond.value) &&
  depthSecond.value[0] === 2 && depthOuter.next().done === true;

var primitiveClosed = 0;
var primitiveOuter = Object.create(Iterator.prototype);
primitiveOuter.next = function() { return { value: 1, done: false }; };
primitiveOuter.return = function() { primitiveClosed++; return {}; };
var primitiveHelper = primitiveOuter.flatMap(function() { return "ab"; });
var primitiveTypeError = false;
try { primitiveHelper.next(); } catch (error) { primitiveTypeError = error instanceof TypeError; }
var primitiveOk = primitiveTypeError && primitiveClosed === 1 && primitiveHelper.next().done === true;

Number.prototype[Symbol.iterator] = function() { return [5, 6].values(); };
var wrapper = [0].values().flatMap(function() { return new Number(2); });
var wrapperOk = wrapper.next().value === 5 && wrapper.next().value === 6 && wrapper.next().done === true;

var counters = [];
var empty = { next: function() { return { done: true }; } };
var counterHelper = [10, 20, 30].values().flatMap(function(value, index) {
  counters.push(index);
  return value === 10 ? empty : [value];
});
var counterOk = counterHelper.next().value === 20 && counterHelper.next().value === 30 &&
  counterHelper.next().done === true && counters.join(",") === "0,1,2";

var innerNextGets = 0;
var cachedInner = {};
Object.defineProperty(cachedInner, "next", {
  get: function() {
    innerNextGets++;
    var cursor = 0;
    return function() { return cursor++ === 0 ? { value: 7, done: false } : { done: true }; };
  }
});
var cached = [0].values().flatMap(function() { return cachedInner; });
var cachedOk = cached.next().value === 7 && cached.next().done === true && innerNextGets === 1;

var mapperError = new Error("mapper");
var mapperCloseCount = 0;
var mapperOuter = Object.create(Iterator.prototype);
mapperOuter.next = function() { return { value: 1, done: false }; };
mapperOuter.return = function() { mapperCloseCount++; throw new Error("close"); };
var mapperThrown;
try { mapperOuter.flatMap(function() { throw mapperError; }).next(); } catch (error) { mapperThrown = error; }
var mapperCloseOk = mapperThrown === mapperError && mapperCloseCount === 1;

var innerError = new Error("inner");
var protocolCloseCount = 0;
var protocolOuter = Object.create(Iterator.prototype);
protocolOuter.next = function() { return { value: 1, done: false }; };
protocolOuter.return = function() { protocolCloseCount++; return {}; };
var protocolThrown;
try {
  protocolOuter.flatMap(function() {
    return { next: function() { throw innerError; } };
  }).next();
} catch (error) { protocolThrown = error; }
var protocolCloseOk = protocolThrown === innerError && protocolCloseCount === 1;

var naturalInnerReturns = 0;
var naturalOuterReturns = 0;
var naturalOuter = Object.create(Iterator.prototype);
var naturalCursor = 0;
naturalOuter.next = function() {
  return naturalCursor++ === 0 ? { value: 1, done: false } : { done: true };
};
naturalOuter.return = function() { naturalOuterReturns++; return {}; };
var naturalInner = {
  next: function() { return { done: true }; },
  return: function() { naturalInnerReturns++; return {}; }
};
var natural = naturalOuter.flatMap(function() { return naturalInner; });
var naturalOk = natural.next().done === true && naturalInnerReturns === 0 && naturalOuterReturns === 0;

var closeOrder = "";
var closeOuter = Object.create(Iterator.prototype);
closeOuter.next = function() { return { value: 1, done: false }; };
closeOuter.return = function() { closeOrder += "o"; return {}; };
var closeInner = {
  next: function() { return { value: 9, done: false }; },
  return: function() { closeOrder += "i"; return {}; }
};
var closeHelper = closeOuter.flatMap(function() { return closeInner; });
closeHelper.next();
var closeResult = closeHelper.return();
var closeOk = closeOrder === "io" && closeResult.done === true && closeHelper.next().done === true;

var innerCloseError = new Error("inner close");
var precedenceOrder = "";
var precedenceOuter = Object.create(Iterator.prototype);
precedenceOuter.next = function() { return { value: 1, done: false }; };
precedenceOuter.return = function() { precedenceOrder += "o"; throw new Error("outer close"); };
var precedenceInner = {
  next: function() { return { value: 1, done: false }; },
  return: function() { precedenceOrder += "i"; throw innerCloseError; }
};
var precedenceHelper = precedenceOuter.flatMap(function() { return precedenceInner; });
precedenceHelper.next();
var precedenceThrown;
try { precedenceHelper.return(); } catch (error) { precedenceThrown = error; }
var precedenceOk = precedenceThrown === innerCloseError && precedenceOrder === "io";

var reentryClosed = 0;
var reentryOuter = Object.create(Iterator.prototype);
reentryOuter.next = function() { return { value: 1, done: false }; };
reentryOuter.return = function() { reentryClosed++; return {}; };
var reentryHelper;
reentryHelper = reentryOuter.flatMap(function() { reentryHelper.next(); return [1]; });
var reentryTypeError = false;
try { reentryHelper.next(); } catch (error) { reentryTypeError = error instanceof TypeError; }
var reentryOk = reentryTypeError && reentryClosed === 1 && reentryHelper.next().done === true;

var chained = [1, 2, 3].values()
  .map(function(value) { return value + 1; })
  .filter(function(value) { return value > 2; })
  .take(2)
  .drop(1)
  .flatMap(function(value) { return [value, value * 10]; });
var chainOk = chained.next().value === 4 && chained.next().value === 40 && chained.next().done === true;

descriptorOk && flattenOk && depthOk && primitiveOk && wrapperOk && counterOk && cachedOk &&
  mapperCloseOk && protocolCloseOk && naturalOk && closeOk && precedenceOk && reentryOk && chainOk;
"#;

#[test]
fn iterator_flat_map_is_stable_for_every_dispatch_batch() {
    assert_iterator_flat_map::<1>(9_061, false);
    assert_iterator_flat_map::<2>(9_062, false);
    assert_iterator_flat_map::<4>(9_064, false);
    assert_iterator_flat_map::<8>(9_068, false);
    assert_iterator_flat_map::<16>(9_076, false);
}

#[test]
fn iterator_flat_map_roots_survive_forced_major_collection() {
    assert_iterator_flat_map::<8>(9_080, true);
}

/// Proves one public next can drain a deep all-native empty-inner chain without Rust recursion.
#[test]
fn flat_map_empty_inner_loops_do_not_grow_the_rust_stack() {
    let source = r#"
var toEmptyWrapper = Object.bind(null, "");
var result = "x".repeat(2000)[Symbol.iterator]().flatMap(toEmptyWrapper).next();
result.done === true && result.value === undefined;
"#;
    let module = compile_iterator_source(source, 9_081);
    let mut isolate = test_isolate_with_heap_spans(512);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_194_304,
                quantum: 4_194_304,
            },
        )
        .expect("large flatMap empty-inner fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "large native flatMap loop returned {outcome:?}"
    );
}

/// Executes flatMap's nested iterator protocol under one dispatch and GC policy.
fn assert_iterator_flat_map<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_iterator_source(ITERATOR_FLAT_MAP_SOURCE, source_id);
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 524_288,
                quantum: 524_288,
            },
        )
        .expect("Iterator.flatMap fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(error) => isolate.native_error_kind(error).ok().flatten(),
        RunOutcome::Completed(_) | RunOutcome::BudgetExhausted => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Compiles one standalone Iterator Helper source fixture.
fn compile_iterator_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("iterator-flat-map-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Iterator.flatMap fixture compiles")
}
