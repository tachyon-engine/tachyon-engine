use super::fixtures::*;
use super::*;
use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

const GENERATOR_SOURCE: &str = r#"
var entered = 0;
function* values(value) {
    entered = entered + 1;
    return value + 1;
}
var functionPrototype = Object.getPrototypeOf(values);
var ownPrototype = values.prototype;
var generatorPrototype = Object.getPrototypeOf(ownPrototype);
var nextMetadata = generatorPrototype.next.name === "next" && generatorPrototype.next.length === 1;
var generator = values(41);
var delayed = entered === 0;
var instanceChain = Object.getPrototypeOf(generator) === ownPrototype;
var functionChain = functionPrototype !== Function.prototype &&
    Object.getPrototypeOf(functionPrototype) === Function.prototype;
var descriptor = Object.getOwnPropertyDescriptor(values, "prototype");
var descriptorOk = descriptor.value === ownPrototype && !descriptor.writable &&
    !descriptor.enumerable && descriptor.configurable;
var first = generator.next();
var second = generator.next();
var third = generator.next();
var completionOk = entered === 1 && first.value === 42 && first.done &&
    second.value === undefined && second.done && third.value === undefined && third.done;

var brandRejected = false;
try { generatorPrototype.next.call({}); }
catch (error) { brandRejected = error instanceof TypeError; }
var newRejected = false;
try { new values(); }
catch (error) { newRejected = error instanceof TypeError; }

var marker = {};
function* fail() { throw marker; }
var failed = fail();
var throwIdentity = false;
try { failed.next(); }
catch (error) { throwIdentity = error === marker; }
var afterThrow = failed.next();

var reentrant;
function* reenter() { return reentrant.next(); }
reentrant = reenter();
var executingRejected = false;
try { reentrant.next(); }
catch (error) { executingRejected = error instanceof TypeError; }
var afterReentry = reentrant.next();

delayed && instanceChain && functionChain && nextMetadata && descriptorOk && completionOk &&
    brandRejected && newRejected && throwIdentity && afterThrow.done &&
    afterThrow.value === undefined && executingRejected && afterReentry.done;
"#;

const GENERATOR_YIELD_SOURCE: &str = r#"
var entered = 0;
function* exchange() {
    entered = entered + 1;
    var first = yield 1;
    var second = yield first + 1;
    return second + 1;
}
var generator = exchange();
var before = entered === 0;
var first = generator.next(999);
var second = generator.next(40);
var third = generator.next(41);
var fourth = generator.next(100);
var exchangeOk = before && entered === 1 && first.value === 1 && !first.done &&
    second.value === 41 && !second.done && third.value === 42 && third.done &&
    fourth.value === undefined && fourth.done;

var finallyValue = 0;
function* protectedYield() {
    try {
        yield 4;
    } finally {
        var injected = yield 5;
        finallyValue = injected;
    }
}
var protectedGenerator = protectedYield();
var protectedFirst = protectedGenerator.next();
var protectedSecond = protectedGenerator.next(10);
var protectedThird = protectedGenerator.next(77);
var finallyOk = protectedFirst.value === 4 && !protectedFirst.done &&
    protectedSecond.value === 5 && !protectedSecond.done && protectedThird.done &&
    protectedThird.value === undefined && finallyValue === 77;

exchangeOk && finallyOk;
"#;

const GENERATOR_ABRUPT_SOURCE: &str = r#"
var marker = {};
var replacement = {};
var entered = 0;
function* neverEntered() { entered = entered + 1; yield 1; }
var startReturnGenerator = neverEntered();
var startReturn = startReturnGenerator.return(11);
var startReturnOk = entered === 0 && startReturn.value === 11 && startReturn.done &&
    startReturnGenerator.next().done;
var startThrowGenerator = neverEntered();
var startThrowOk = false;
try { startThrowGenerator.throw(marker); }
catch (error) { startThrowOk = error === marker; }
startThrowOk = startThrowOk && entered === 0 && startThrowGenerator.next().done;

function* completedBody() { return 3; }
var completedGenerator = completedBody();
var completedFirst = completedGenerator.next();
var completedReturn = completedGenerator.return(12);
var completedThrowOk = false;
try { completedGenerator.throw(marker); }
catch (error) { completedThrowOk = error === marker; }
var completedOk = completedFirst.value === 3 && completedFirst.done &&
    completedReturn.value === 12 && completedReturn.done && completedThrowOk;

function* caughtThrow() {
    try { yield 1; }
    catch (error) { yield error; }
    return 4;
}
var caughtGenerator = caughtThrow();
var caughtFirst = caughtGenerator.next();
var caughtSecond = caughtGenerator.throw(marker);
var caughtThird = caughtGenerator.next();
var caughtOk = caughtFirst.value === 1 && !caughtFirst.done &&
    caughtSecond.value === marker && !caughtSecond.done &&
    caughtThird.value === 4 && caughtThird.done;

function* returnThroughFinally() {
    try { yield 1; }
    finally { yield 2; }
}
var returnGenerator = returnThroughFinally();
var returnFirst = returnGenerator.next();
var returnSecond = returnGenerator.return(13);
var returnThird = returnGenerator.next();
var returnFinallyOk = returnFirst.value === 1 && !returnFirst.done &&
    returnSecond.value === 2 && !returnSecond.done &&
    returnThird.value === 13 && returnThird.done;

function* throwThroughFinally() {
    try { yield 1; }
    finally { yield 2; }
}
var throwGenerator = throwThroughFinally();
var throwFirst = throwGenerator.next();
var throwSecond = throwGenerator.throw(marker);
var throwThirdOk = false;
try { throwGenerator.next(); }
catch (error) { throwThirdOk = error === marker; }
var throwFinallyOk = throwFirst.value === 1 && !throwFirst.done &&
    throwSecond.value === 2 && !throwSecond.done && throwThirdOk &&
    throwGenerator.next().done;

function* overrideReturn() {
    try { yield 1; }
    finally { return 14; }
}
var overrideReturnGenerator = overrideReturn();
overrideReturnGenerator.next();
var overrideReturnResult = overrideReturnGenerator.return(15);
var overrideReturnOk = overrideReturnResult.value === 14 && overrideReturnResult.done;
function* overrideThrow() {
    try { yield 1; }
    finally { throw replacement; }
}
var overrideThrowGenerator = overrideThrow();
overrideThrowGenerator.next();
var overrideThrowOk = false;
try { overrideThrowGenerator.return(16); }
catch (error) { overrideThrowOk = error === replacement; }

var reentrantReturn;
function* reenterReturn() { return reentrantReturn.return(1); }
reentrantReturn = reenterReturn();
var executingReturnOk = false;
try { reentrantReturn.next(); }
catch (error) { executingReturnOk = error instanceof TypeError; }
var reentrantThrow;
function* reenterThrow() { return reentrantThrow.throw(marker); }
reentrantThrow = reenterThrow();
var executingThrowOk = false;
try { reentrantThrow.next(); }
catch (error) { executingThrowOk = error instanceof TypeError; }

var prototype = Object.getPrototypeOf(Object.getPrototypeOf(neverEntered()));
var metadataOk = prototype.return.name === "return" && prototype.return.length === 1 &&
    prototype.throw.name === "throw" && prototype.throw.length === 1;
var returnBrandOk = false;
var throwBrandOk = false;
try { prototype.return.call({}, 1); }
catch (error) { returnBrandOk = error instanceof TypeError; }
try { prototype.throw.call({}, marker); }
catch (error) { throwBrandOk = error instanceof TypeError; }

startReturnOk && startThrowOk && completedOk && caughtOk && returnFinallyOk &&
    throwFinallyOk && overrideReturnOk && overrideThrowOk && executingReturnOk &&
    executingThrowOk && metadataOk && returnBrandOk && throwBrandOk;
"#;

const GENERATOR_DELEGATE_SOURCE: &str = r#"
function iterable(iterator) {
    var value = {};
    value[Symbol.iterator] = function() { return iterator; };
    return value;
}

var firstResult = { value: 1, done: false };
var nextCalls = 0;
var firstArgumentOk = false;
var nextIterator = {};
nextIterator.next = function(value) {
    nextCalls = nextCalls + 1;
    if (nextCalls === 1) {
        firstArgumentOk = value === undefined;
        return firstResult;
    }
    return { value: value + 1, done: true };
};
function* nextDelegate() { return yield* iterable(nextIterator); }
var nextGenerator = nextDelegate();
var nextFirst = nextGenerator.next(999);
var nextSecond = nextGenerator.next(7);
var nextOk = nextFirst === firstResult && firstArgumentOk && nextCalls === 2 &&
    nextSecond.value === 8 && nextSecond.done;

var returnResult = { value: 2, done: false };
var returnCalls = 0;
var returnNextCalls = 0;
var returnIterator = {};
returnIterator.next = function(value) {
    returnNextCalls = returnNextCalls + 1;
    if (returnNextCalls === 1) return { value: 1, done: false };
    return { value: value + 10, done: true };
};
returnIterator.return = function(value) {
    returnCalls = returnCalls + 1;
    return value === 5 ? returnResult : { value: 99, done: true };
};
function* returnDelegate() { return yield* iterable(returnIterator); }
var returnGenerator = returnDelegate();
returnGenerator.next();
var returnFirst = returnGenerator.return(5);
var returnSecond = returnGenerator.next(6);
var returnOk = returnFirst === returnResult && returnCalls === 1 &&
    returnNextCalls === 2 && returnSecond.value === 16 && returnSecond.done;

var throwResult = { value: 3, done: false };
var throwCalls = 0;
var throwNextCalls = 0;
var throwMarker = {};
var throwIterator = {};
throwIterator.next = function(value) {
    throwNextCalls = throwNextCalls + 1;
    if (throwNextCalls === 1) return { value: 1, done: false };
    return { value: value, done: true };
};
throwIterator.throw = function(value) {
    throwCalls = throwCalls + 1;
    return value === throwMarker ? throwResult : { value: 100, done: true };
};
function* throwDelegate() { return yield* iterable(throwIterator); }
var throwGenerator = throwDelegate();
throwGenerator.next();
var throwFirst = throwGenerator.throw(throwMarker);
var throwSecond = throwGenerator.next(9);
var throwOk = throwFirst === throwResult && throwCalls === 1 && throwNextCalls === 2 &&
    throwSecond.value === 9 && throwSecond.done;

var completedReturnIterator = {};
completedReturnIterator.next = function() { return { value: 1, done: false }; };
completedReturnIterator.return = function(value) { return { value: value + 1, done: true }; };
function* completedReturnDelegate() { return yield* iterable(completedReturnIterator); }
var completedReturnGenerator = completedReturnDelegate();
completedReturnGenerator.next();
var completedReturn = completedReturnGenerator.return(20);
var completedReturnOk = completedReturn.value === 21 && completedReturn.done;

var completedThrowIterator = {};
completedThrowIterator.next = function() { return { value: 1, done: false }; };
completedThrowIterator.throw = function(value) { return { value: value, done: true }; };
function* completedThrowDelegate() { return yield* iterable(completedThrowIterator); }
var completedThrowGenerator = completedThrowDelegate();
completedThrowGenerator.next();
var completedThrow = completedThrowGenerator.throw(throwMarker);
var completedThrowOk = completedThrow.value === throwMarker && completedThrow.done;

var missingReturnIterator = {};
missingReturnIterator.next = function() { return { value: 1, done: false }; };
function* missingReturnDelegate() { return yield* iterable(missingReturnIterator); }
var missingReturnGenerator = missingReturnDelegate();
missingReturnGenerator.next();
var missingReturn = missingReturnGenerator.return(22);
var missingReturnOk = missingReturn.value === 22 && missingReturn.done;

var closeCalls = 0;
var missingThrowIterator = {};
missingThrowIterator.next = function() { return { value: 1, done: false }; };
missingThrowIterator.return = function() { closeCalls = closeCalls + 1; return {}; };
function* missingThrowDelegate() { return yield* iterable(missingThrowIterator); }
var missingThrowGenerator = missingThrowDelegate();
missingThrowGenerator.next();
var missingThrowOk = false;
try { missingThrowGenerator.throw(throwMarker); }
catch (error) { missingThrowOk = error instanceof TypeError && closeCalls === 1; }

var primitiveIterator = {};
primitiveIterator.next = function() { return 1; };
function* primitiveDelegate() { return yield* iterable(primitiveIterator); }
var primitiveOk = false;
try { primitiveDelegate().next(); }
catch (error) { primitiveOk = error instanceof TypeError; }

var finallyCount = 0;
var protectedIterator = {};
protectedIterator.next = function() { return { value: 1, done: false }; };
protectedIterator.return = function(value) { return { value: value, done: true }; };
function* protectedDelegate() {
    try { return yield* iterable(protectedIterator); }
    finally { finallyCount = finallyCount + 1; }
}
var protectedGenerator = protectedDelegate();
protectedGenerator.next();
var protectedResult = protectedGenerator.return(23);
var protectedOk = protectedResult.value === 23 && protectedResult.done && finallyCount === 1;

function* innerDelegate() { yield 30; return 31; }
function* outerDelegate() { return yield* innerDelegate(); }
var nestedGenerator = outerDelegate();
var nestedFirst = nestedGenerator.next();
var nestedSecond = nestedGenerator.next();
var nestedOk = nestedFirst.value === 30 && !nestedFirst.done &&
    nestedSecond.value === 31 && nestedSecond.done;

nextOk && returnOk && throwOk && completedReturnOk && completedThrowOk &&
    missingReturnOk && missingThrowOk && primitiveOk && protectedOk && nestedOk;
"#;

const GENERATOR_DELEGATE_ERRORS_SOURCE: &str = r#"
function iterable(iterator) {
    var value = {};
    value[Symbol.iterator] = function() { return iterator; };
    return value;
}
function start(iterator) {
    function* delegate() { return yield* iterable(iterator); }
    var generator = delegate();
    generator.next();
    return generator;
}

var marker = {};
var absentIterator = {};
absentIterator.next = function() { return { value: 1, done: false }; };
var absentOk = false;
try { start(absentIterator).throw(marker); }
catch (error) { absentOk = error instanceof TypeError; }

var primitiveCloseCalls = 0;
var primitiveCloseIterator = {};
primitiveCloseIterator.next = function() { return { value: 1, done: false }; };
primitiveCloseIterator.return = function() { primitiveCloseCalls = primitiveCloseCalls + 1; return 1; };
var primitiveCloseOk = false;
try { start(primitiveCloseIterator).throw(marker); }
catch (error) { primitiveCloseOk = error instanceof TypeError && primitiveCloseCalls === 1; }

var closeGetterMarker = {};
var closeGetterIterator = {
    get return() { throw closeGetterMarker; }
};
closeGetterIterator.next = function() { return { value: 1, done: false }; };
var closeGetterOk = false;
try { start(closeGetterIterator).throw(marker); }
catch (error) { closeGetterOk = error === closeGetterMarker; }

var closeCallMarker = {};
var closeCallIterator = {};
closeCallIterator.next = function() { return { value: 1, done: false }; };
closeCallIterator.return = function() { throw closeCallMarker; };
var closeCallOk = false;
try { start(closeCallIterator).throw(marker); }
catch (error) { closeCallOk = error === closeCallMarker; }

var unexpectedCloseCalls = 0;
var throwGetterMarker = {};
var throwGetterIterator = {
    get throw() { throw throwGetterMarker; }
};
throwGetterIterator.next = function() { return { value: 1, done: false }; };
throwGetterIterator.return = function() { unexpectedCloseCalls = unexpectedCloseCalls + 1; return {}; };
var throwGetterOk = false;
try { start(throwGetterIterator).throw(marker); }
catch (error) { throwGetterOk = error === throwGetterMarker && unexpectedCloseCalls === 0; }

var throwCallMarker = {};
var throwCallIterator = {};
throwCallIterator.next = function() { return { value: 1, done: false }; };
throwCallIterator.throw = function() { throw throwCallMarker; };
throwCallIterator.return = function() { unexpectedCloseCalls = unexpectedCloseCalls + 1; return {}; };
var throwCallOk = false;
try { start(throwCallIterator).throw(marker); }
catch (error) { throwCallOk = error === throwCallMarker && unexpectedCloseCalls === 0; }

var primitiveThrowIterator = {};
primitiveThrowIterator.next = function() { return { value: 1, done: false }; };
primitiveThrowIterator.throw = function() { return 1; };
var primitiveThrowOk = false;
try { start(primitiveThrowIterator).throw(marker); }
catch (error) { primitiveThrowOk = error instanceof TypeError; }

var primitiveReturnIterator = {};
primitiveReturnIterator.next = function() { return { value: 1, done: false }; };
primitiveReturnIterator.return = function() { return 1; };
var primitiveReturnOk = false;
try { start(primitiveReturnIterator).return(marker); }
catch (error) { primitiveReturnOk = error instanceof TypeError; }

var caughtFinally = 0;
function* caughtDelegate() {
    try { return yield* iterable(absentIterator); }
    catch (error) { return error instanceof TypeError; }
    finally { caughtFinally = caughtFinally + 1; }
}
var caughtGenerator = caughtDelegate();
caughtGenerator.next();
var caughtResult = caughtGenerator.throw(marker);
var caughtOk = caughtResult.value === true && caughtResult.done && caughtFinally === 1;

absentOk && primitiveCloseOk && closeGetterOk && closeCallOk && throwGetterOk &&
    throwCallOk && primitiveThrowOk && primitiveReturnOk && caughtOk;
"#;

#[test]
fn generator_return_slice_runs_for_every_dispatch_batch() {
    assert_generator_source::<1>(false);
    assert_generator_source::<2>(false);
    assert_generator_source::<4>(false);
    assert_generator_source::<8>(false);
    assert_generator_source::<16>(false);
}

#[test]
fn generator_state_and_arguments_survive_forced_major_collection() {
    assert_generator_source::<8>(true);
}

#[test]
fn generator_yield_and_next_value_run_for_every_dispatch_batch() {
    assert_generator_yield_source::<1>(false);
    assert_generator_yield_source::<2>(false);
    assert_generator_yield_source::<4>(false);
    assert_generator_yield_source::<8>(false);
    assert_generator_yield_source::<16>(false);
}

#[test]
fn generator_yield_state_survives_forced_major_collection() {
    assert_generator_yield_source::<8>(true);
}

#[test]
fn generator_return_and_throw_run_for_every_dispatch_batch() {
    assert_generator_abrupt_source::<1>(false);
    assert_generator_abrupt_source::<2>(false);
    assert_generator_abrupt_source::<4>(false);
    assert_generator_abrupt_source::<8>(false);
    assert_generator_abrupt_source::<16>(false);
}

#[test]
fn generator_abrupt_completions_survive_forced_major_collection() {
    assert_generator_abrupt_source::<8>(true);
}

#[test]
fn generator_delegate_protocol_runs_for_every_dispatch_batch() {
    assert_generator_delegate_source::<1>(false);
    assert_generator_delegate_source::<2>(false);
    assert_generator_delegate_source::<4>(false);
    assert_generator_delegate_source::<8>(false);
    assert_generator_delegate_source::<16>(false);
}

#[test]
fn generator_delegate_state_survives_forced_major_collection() {
    assert_generator_delegate_source::<8>(true);
}

#[test]
fn generator_delegate_error_precedence_runs_for_every_dispatch_batch() {
    assert_generator_delegate_errors::<1>(false);
    assert_generator_delegate_errors::<2>(false);
    assert_generator_delegate_errors::<4>(false);
    assert_generator_delegate_errors::<8>(false);
    assert_generator_delegate_errors::<16>(false);
}

#[test]
fn generator_delegate_error_roots_survive_forced_major_collection() {
    assert_generator_delegate_errors::<8>(true);
}

/// Repeats both abrupt kinds without growing the native stack or retaining completed Fibers.
#[test]
fn generator_abrupt_large_loop_uses_constant_native_stack() {
    let source = r#"
var marker = {};
function* value() { yield 1; }
var index = 0;
var valid = true;
while (index < 512) {
    var returned = value();
    returned.next();
    var result = returned.return(index);
    valid = valid && result.done && result.value === index;
    var thrown = value();
    thrown.next();
    try { thrown.throw(marker); valid = false; }
    catch (error) { valid = valid && error === marker; }
    index = index + 1;
}
valid;
"#;
    let (_, outcome) = execute_generator_fixture_with_heap(2_503, source, 64);
    assert_eq!(outcome.as_immediate(), Some(Immediate::True));
}

/// Repeats nested delegated Fibers without growing the native Rust call stack.
#[test]
fn generator_delegate_large_loop_uses_constant_native_stack() {
    let source = r#"
function* inner(value) { yield value; return value + 1; }
function* outer(value) { return yield* inner(value); }
var index = 0;
var valid = true;
while (index < 512) {
    var generator = outer(index);
    var first = generator.next();
    var second = generator.next();
    valid = valid && first.value === index && !first.done &&
        second.value === index + 1 && second.done;
    index = index + 1;
}
valid;
"#;
    let (_, outcome) = execute_generator_fixture_with_heap(2_504, source, 64);
    assert_eq!(outcome.as_immediate(), Some(Immediate::True));
}

/// Exercises repeated Fiber ownership transfer without growing the native Rust call stack.
#[test]
fn generator_large_resume_loop_uses_constant_native_stack() {
    let source = r#"
function* count(limit) {
    var index = 0;
    while (index < limit) {
        yield index;
        index = index + 1;
    }
    return index;
}
var generator = count(512);
var expected = 0;
var valid = true;
while (expected < 512) {
    var result = generator.next();
    valid = valid && !result.done && result.value === expected;
    expected = expected + 1;
}
var completed = generator.next();
valid && completed.done && completed.value === 512;
"#;
    let (_, outcome) = execute_generator_fixture(2_502, source);
    assert_eq!(outcome.as_immediate(), Some(Immediate::True));
}

/// Verifies exact suspended storage and constant-space completed `.next()` behavior at large argc.
#[test]
fn generator_large_activation_is_exact_then_released_before_repeated_next() {
    const ARGUMENT_COUNT: usize = 256;
    let arguments = (0..ARGUMENT_COUNT)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let suspended_source = format!(
        "function* many() {{ return arguments.length; }}\nvar generator = many({arguments});\ngenerator;"
    );
    let (mut isolate, suspended) = execute_generator_fixture(2_500, &suspended_source);
    assert_eq!(
        isolate
            .generator_retained_argument_capacity(suspended)
            .expect("suspended generator has inspectable retained arguments"),
        ARGUMENT_COUNT,
        "suspended-start backing is an exact boxed argument list"
    );

    let completed_source = format!(
        "function* many() {{ return arguments.length; }}\n\
         var generator = many({arguments});\n\
         var first = generator.next();\n\
         if (first.value !== {ARGUMENT_COUNT} || !first.done) {{ throw 'first'; }}\n\
         var index = 0;\n\
         while (index < 1024) {{\n\
             var next = generator.next();\n\
             if (next.value !== undefined || !next.done) {{ throw 'repeat'; }}\n\
             index = index + 1;\n\
         }}\n\
         generator;"
    );
    let (mut isolate, completed) = execute_generator_fixture(2_501, &completed_source);
    assert_eq!(
        isolate
            .generator_retained_argument_capacity(completed)
            .expect("completed generator remains inspectable"),
        0,
        "completed generator releases its argument-prefix root"
    );
}

/// Compiles and executes the complete return-only generator contract under one dispatch policy.
fn assert_generator_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_400 + N as u32),
                SourceName::new("generator-return-slice"),
                MediaType::JavaScript,
                Arc::from(GENERATOR_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generator fixture compiles");
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
                fuel: 100_000,
                quantum: 100_000,
            },
        )
        .expect("generator fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles and runs the complete ordinary-yield contract under one dispatch/collection policy.
fn assert_generator_yield_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_600 + N as u32),
                SourceName::new("generator-yield-slice"),
                MediaType::JavaScript,
                Arc::from(GENERATOR_YIELD_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generator yield fixture compiles");
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
                fuel: 200_000,
                quantum: 200_000,
            },
        )
        .expect("generator yield fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "yield dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles and runs abrupt injection under one dispatch and collection policy.
fn assert_generator_abrupt_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_700 + N as u32),
                SourceName::new("generator-abrupt-slice"),
                MediaType::JavaScript,
                Arc::from(GENERATOR_ABRUPT_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generator abrupt fixture compiles");
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
                fuel: 300_000,
                quantum: 300_000,
            },
        )
        .expect("generator abrupt fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "abrupt dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles and runs delegated next/return/throw forwarding under one dispatch policy.
fn assert_generator_delegate_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_800 + N as u32),
                SourceName::new("generator-delegate-slice"),
                MediaType::JavaScript,
                Arc::from(GENERATOR_DELEGATE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generator delegate fixture compiles");
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
                fuel: 500_000,
                quantum: 500_000,
            },
        )
        .expect("generator delegate fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "delegate dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Runs delegated close/error precedence and catch/finally routing under one dispatch policy.
fn assert_generator_delegate_errors<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_900 + N as u32),
                SourceName::new("generator-delegate-errors"),
                MediaType::JavaScript,
                Arc::from(GENERATOR_DELEGATE_ERRORS_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generator delegate error fixture compiles");
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
                fuel: 500_000,
                quantum: 500_000,
            },
        )
        .expect("generator delegate error fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "delegate error batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes a generated stress fixture and returns both the isolate and its final generator value.
fn execute_generator_fixture(source_id: u32, source: &str) -> (Isolate, Value) {
    execute_generator_fixture_with_heap(source_id, source, 9)
}

/// Executes a stress fixture under an explicit heap bound sized to its intentional object churn.
fn execute_generator_fixture_with_heap(
    source_id: u32,
    source: &str,
    heap_spans: usize,
) -> (Isolate, Value) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("generator-activation-stress"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("generator stress fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(heap_spans * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("generator stress isolate descriptors register");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 500_000,
                quantum: 500_000,
            },
        )
        .expect("generator stress fixture executes");
    let RunOutcome::Completed(value) = outcome else {
        panic!("generator stress fixture did not complete: {outcome:?}");
    };
    (isolate, value)
}
