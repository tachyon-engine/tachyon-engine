use super::*;
use tachyon_bytecode::CompiledModule;

/// Compiles one Promise fixture whose state will be inspected by a later source unit.
fn compile_promise_fixture(
    source_id: u32,
    name: &'static str,
    text: &'static str,
) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new(name),
                MediaType::JavaScript,
                Arc::from(text),
            ),
            CompileOptions::default(),
        )
        .unwrap()
}

#[test]
/// Publishes Promise and preserves intrinsic Promise.resolve identity without running jobs.
fn promise_static_resolution_allocates_branded_objects() {
    for (source_id, source) in [
        (
            1_022,
            "let p = Promise.resolve(1); Promise.resolve(p) === p;",
        ),
        (1_023, "Promise.resolve(1) !== Promise.reject(2);"),
        (
            1_024,
            "Object.getPrototypeOf(Promise.resolve(1)) === Promise.prototype;",
        ),
        (
            1_065,
            "var p = new Promise(function() {}); p.constructor = null; Promise.resolve(p) !== p;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(tachyon_value::Immediate::True),
            "failed source: {source}",
        );
    }
}

#[test]
/// Uses the receiver constructor and validates both captured functions for generic rejection.
fn promise_static_reject_builds_generic_capabilities() {
    for (source_id, source) in [
        (
            1_054,
            "function C(executor) { executor(function() {}, function(reason) { if (reason !== 7) throw 91; }); return { marker: 8 }; } var result = Promise.reject.call(C, 7); result.marker === 8;",
        ),
        (
            1_055,
            "function C(executor) { executor(undefined, function() {}); return {}; } var threw = false; try { Promise.reject.call(C, 1); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(tachyon_value::Immediate::True),
            "failed source: {source}",
        );
    }
}

#[test]
/// Calls Promise executors synchronously and converts executor throws into rejection.
fn promise_constructor_uses_resolving_functions_and_consumes_executor_throw() {
    for (source_id, source) in [
        (
            1_025,
            "let calls = 0; let p = new Promise(function(resolve, reject) { calls = calls + 1; resolve(3); reject(4); }); calls === 1 && p instanceof Promise;",
        ),
        (
            1_026,
            "let calls = 0; let p = new Promise(function() { calls = calls + 1; throw 7; }); calls === 1 && p instanceof Promise;",
        ),
        (
            1_027,
            "let caught = false; try { new Promise(1); } catch (error) { caught = error instanceof TypeError; } caught;",
        ),
        (
            1_028,
            "let resolve, reject, count; new Promise(function(a, b) { resolve = a; reject = b; count = arguments.length; }); typeof resolve === 'function' && resolve.length === 1 && resolve.name === '' && typeof reject === 'function' && reject.length === 1 && count === 2;",
        ),
        (
            1_029,
            "let captured; new Promise(function() { 'use strict'; captured = this; }); captured === undefined;",
        ),
        (
            1_030,
            "var resolve, reject, count; new Promise(function(a, b) { resolve = a; reject = b; count = arguments.length; }); typeof resolve === 'function' && resolve.length === 1 && resolve.name === '' && typeof reject === 'function' && reject.length === 1 && count === 2;",
        ),
        (
            1_031,
            "function Target() {} Target.prototype = {}; let p = Reflect.construct(Promise, [function() {}], Target); Object.getPrototypeOf(p) === Target.prototype;",
        ),
        (
            1_032,
            "let threw = false; let p; try { p = new Promise(Array.prototype.push); } catch (error) { threw = true; } !threw && p instanceof Promise;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(tachyon_value::Immediate::True),
            "failed source: {source}",
        );
    }
}

#[test]
/// Propagates abrupt completion from a custom Promise @@species constructor unchanged.
fn promise_then_enters_custom_species_constructor() {
    for (source_id, source) in [
        (
            1_043,
            "var BadCtor = function() { throw 9; }; Object.defineProperty(Promise, Symbol.species, { value: BadCtor }); var caught = 0; try { Promise.resolve().then(); } catch (error) { caught = error; } caught === 9;",
        ),
        (
            1_044,
            "function TestError(message) { if (!(this instanceof TestError)) return new TestError(message); this.message = message || ''; } var BadCtor = function() { throw new TestError(); }; Object.defineProperty(Promise, Symbol.species, { value: BadCtor }); var caught; try { Promise.resolve().then(); } catch (error) { caught = error; } caught.constructor === TestError;",
        ),
        (
            1_045,
            "var descriptor = Object.getOwnPropertyDescriptor(Promise, Symbol.species); typeof descriptor.get === 'function' && descriptor.set === undefined && descriptor.configurable === true && descriptor.enumerable === false;",
        ),
        (
            1_046,
            "var BadCtor = function() {}; Object.defineProperty(Promise, Symbol.species, { value: BadCtor }); var descriptor = Object.getOwnPropertyDescriptor(Promise, Symbol.species); descriptor.value === BadCtor && descriptor.configurable === true;",
        ),
        (
            1_047,
            "var original = Object.getOwnPropertyDescriptor(Promise, Symbol.species); var BadCtor = function() { throw 9; }; Object.defineProperty(Promise, Symbol.species, { value: BadCtor }); var caught = 0; try { Promise.resolve().then(); } catch (error) { caught = error; } Object.defineProperty(Promise, Symbol.species, original); caught === 9 && Promise[Symbol.species] === Promise;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(tachyon_value::Immediate::True),
            "failed source: {source}",
        );
    }
}

#[test]
/// Enforces generic NewPromiseCapability executor capture and constructor result validation.
fn promise_then_builds_generic_species_capability() {
    for (source_id, source) in [
        (
            1_048,
            "function C(executor) { executor(function() {}, function() {}); this.marker = 7; } Object.defineProperty(Promise, Symbol.species, { value: C }); var result = new Promise(function() {}).then(); result.marker === 7;",
        ),
        (
            1_049,
            "function C(executor) {} Object.defineProperty(Promise, Symbol.species, { value: C }); var threw = false; try { new Promise(function() {}).then(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_050,
            "function C(executor) { executor(1, 2); } Object.defineProperty(Promise, Symbol.species, { value: C }); var threw = false; try { new Promise(function() {}).then(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_051,
            "function C(executor) { executor(function() {}, function() {}); executor(function() {}, function() {}); } Object.defineProperty(Promise, Symbol.species, { value: C }); var threw = false; try { new Promise(function() {}).then(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_052,
            "function C(executor) { executor(function(value) { if (value !== 7) throw 91; }, function() { throw 92; }); return {}; } Object.defineProperty(Promise, Symbol.species, { value: C }); Promise.resolve(4).then(function(value) { return value + 3; }); true;",
        ),
        (
            1_053,
            "function C(executor) { executor(function() { throw 93; }, function(value) { if (value !== 8) throw 94; }); return {}; } Object.defineProperty(Promise, Symbol.species, { value: C }); Promise.reject(8).then(); true;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(tachyon_value::Immediate::True),
            "failed source: {source}",
        );
    }
}

#[test]
/// Drains settled, pending, chained, and throwing reactions before the next source unit.
fn promise_checkpoint_drains_nested_reactions_in_fifo_order() {
    let setup = compile_promise_fixture(
        1_033,
        "promise-checkpoint",
        "var trace = 0; var resolvePending; Promise.resolve(2).then(function(value) { trace = trace * 10 + value; return value + 1; }).then(function(value) { trace = trace * 10 + value; }); var pending = new Promise(function(resolve) { resolvePending = resolve; }); pending.then(function(value) { trace = trace * 10 + value; }); resolvePending(4); Promise.resolve(1).then(function() { throw 5; }).catch(function(reason) { trace = trace * 10 + reason; });",
    );
    let probe = compile_promise_fixture(1_034, "promise-checkpoint", "trace;");
    let mut isolate = test_isolate();
    assert!(matches!(
        isolate.execute(
            &setup,
            ExecutionBudget {
                fuel: 512,
                quantum: 512
            }
        ),
        Ok(RunOutcome::Completed(_))
    ));
    assert!(matches!(
        isolate.execute(&probe, ExecutionBudget { fuel: 64, quantum: 64 }),
        Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(2_435)
    ));
}

#[test]
/// Preserves custom derived capability identity while delivering the fulfilled reaction value.
fn promise_reaction_uses_custom_class_capability_without_cycle_rejection() {
    let setup = compile_promise_fixture(
        1_054,
        "promise-derived-capability",
        "var createBadPromise = false; var object = {}; var status = 0; var returned = false; class P extends Promise { constructor(executor) { if (createBadPromise) { executor(function(value) { status = value === object ? 1 : 2; }, function() { status = 3; }); return object; } return super(executor); } } var promise = P.resolve(object); createBadPromise = true; var result = promise.then(); createBadPromise = false; returned = result === object;",
    );
    let probe = compile_promise_fixture(
        1_055,
        "promise-derived-capability",
        "status * 10 + (returned ? 1 : 0);",
    );
    let mut isolate = test_isolate();
    assert!(matches!(
        isolate.execute(
            &setup,
            ExecutionBudget {
                fuel: 512,
                quantum: 512
            }
        ),
        Ok(RunOutcome::Completed(_))
    ));
    let outcome = isolate.execute(
        &probe,
        ExecutionBudget {
            fuel: 64,
            quantum: 64,
        },
    );
    assert!(
        matches!(outcome, Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(11)),
        "unexpected derived capability state: {outcome:?}"
    );
}

#[test]
/// Applies derived constructor object-return and Promise-super initialization rules.
fn derived_promise_class_constructor_preserves_branch_and_return_semantics() {
    for (source_id, source) in [
        (
            1_056,
            "var flag = true; var object = {}; class P extends Promise { constructor(executor) { if (flag) return object; return super(executor); } } new P(function() {}) === object;",
        ),
        (
            1_057,
            "var P = class extends Promise { constructor(executor) { return super(executor); } }; var value = new P(function() {}); value instanceof P && value instanceof Promise;",
        ),
        (
            1_058,
            "class P extends Promise { constructor(executor) { return super(executor); } } var descriptor = Object.getOwnPropertyDescriptor(P, 'prototype'); descriptor.writable === false && descriptor.enumerable === false && descriptor.configurable === false && P.prototype.constructor === P && Object.getPrototypeOf(P) === Promise && Object.getPrototypeOf(P.prototype) === Promise.prototype;",
        ),
        (
            1_059,
            "class P extends Promise { constructor(executor) { return super(executor); } } var threw = false; try { P(function() {}); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_060,
            "class P extends Promise { constructor() { return 1; } } var threw = false; try { new P(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_061,
            "class P extends Promise { constructor() { return undefined; } } var threw = false; try { new P(); } catch (error) { threw = error instanceof ReferenceError; } threw;",
        ),
        (
            1_062,
            "class P extends Promise { constructor() { this.value = 1; } } var threw = false; try { new P(); } catch (error) { threw = error instanceof ReferenceError; } threw;",
        ),
        (
            1_063,
            "var calls = 0; class P extends Promise { constructor() { super(function() { calls = calls + 1; }); super(function() { calls = calls + 1; }); } } var threw = false; try { new P(); } catch (error) { threw = error instanceof ReferenceError; } threw && calls === 2;",
        ),
        (
            1_064,
            "function Other() { this.marker = 9; } class P extends Promise { constructor() { super(); } } Object.setPrototypeOf(P, Other); var value = new P(); value.marker === 9 && value instanceof P;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(tachyon_value::Immediate::True),
            "failed source: {source}",
        );
    }
}

#[test]
/// Installs strict non-constructible instance/static methods with class descriptors.
fn derived_class_methods_publish_callable_semantics() {
    let source = "class P extends Promise { constructor(executor) { super(executor); } value() { return this.marker; } static make(executor) { return new this(executor); } } P.prototype.marker = 7; var instance = P.make(function() {}); var method = P.prototype.value; var instanceDescriptor = Object.getOwnPropertyDescriptor(P.prototype, 'value'); var staticDescriptor = Object.getOwnPropertyDescriptor(P, 'make'); var threw = false; try { new method(); } catch (error) { threw = error instanceof TypeError; } instance.value() === 7 && method.name === 'value' && !Object.hasOwn(method, 'prototype') && instanceDescriptor.writable === true && instanceDescriptor.enumerable === false && instanceDescriptor.configurable === true && staticDescriptor.enumerable === false && threw;";
    assert_eq!(
        execute_source(1_066, source).as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Keeps sibling Promise chains in FIFO order while each handler appends another reaction.
fn promise_checkpoint_preserves_sibling_chain_order() {
    let setup = compile_promise_fixture(
        1_035,
        "promise-chain-order",
        "var sequence = []; var p = Promise.resolve(); sequence.push(1); p.then(function() { sequence.push(3); }).then(function() { sequence.push(5); }).then(function() { sequence.push(7); }); p.then(function() { sequence.push(4); }).then(function() { sequence.push(6); }).then(function() { sequence.push(8); }); sequence.push(2);",
    );
    let probe = compile_promise_fixture(
        1_036,
        "promise-chain-order",
        "sequence.length === 8 ? sequence[0] * 10000000 + sequence[1] * 1000000 + sequence[2] * 100000 + sequence[3] * 10000 + sequence[4] * 1000 + sequence[5] * 100 + sequence[6] * 10 + sequence[7] : -sequence.length;",
    );
    let mut isolate = test_isolate();
    assert!(matches!(
        isolate.execute(
            &setup,
            ExecutionBudget {
                fuel: 512,
                quantum: 512
            }
        ),
        Ok(RunOutcome::Completed(_))
    ));
    let outcome = isolate.execute(
        &probe,
        ExecutionBudget {
            fuel: 64,
            quantum: 64,
        },
    );
    assert!(
        matches!(outcome, Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(12_345_678)),
        "unexpected sibling-chain trace: {outcome:?}",
    );
}

#[test]
/// Assimilates getter-backed thenables and ignores throws after their first resolve call.
fn promise_resolution_assimilates_thenables_at_each_reaction_boundary() {
    let setup = compile_promise_fixture(
        1_037,
        "promise-thenable-resolution",
        "var trace = 0; var thenable = {}; Object.defineProperty(thenable, 'then', { get: function() { trace = trace * 10 + 1; return function(resolve, reject) { trace = trace * 10 + 2; resolve(3); reject(4); throw 5; }; } }); Promise.resolve(0).then(function() { return thenable; }).then(function(value) { trace = trace * 10 + value; }, function() { trace = -1; });",
    );
    let probe = compile_promise_fixture(1_038, "promise-thenable-resolution", "trace;");
    let mut isolate = test_isolate();
    assert!(matches!(
        isolate.execute(
            &setup,
            ExecutionBudget {
                fuel: 512,
                quantum: 512
            }
        ),
        Ok(RunOutcome::Completed(_))
    ));
    assert!(matches!(
        isolate.execute(&probe, ExecutionBudget { fuel: 64, quantum: 64 }),
        Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(123)
    ));
}
