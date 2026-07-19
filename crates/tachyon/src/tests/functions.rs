use super::*;

#[test]
fn logical_not_uses_the_shared_truthiness_contract() {
    let source = SourceText::new(
        SourceId::new(10),
        SourceName::new("embedded-input"),
        MediaType::JavaScript,
        Arc::from("!0;"),
    );
    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let outcome = test_isolate()
        .execute(
            &module,
            ExecutionBudget {
                fuel: 3,
                quantum: 3,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(tachyon_value::Immediate::True)
    ));
}

#[test]
fn logical_expressions_preserve_values_and_skip_right_hand_side_effects() {
    assert_eq!(execute_source(19, "0 || 7;").as_i32(), Some(7));
    assert_eq!(execute_source(20, "5 && 7;").as_i32(), Some(7));
    assert_eq!(execute_source(21, "null ?? 9;").as_i32(), Some(9));
    assert_eq!(execute_source(22, "4 ?? 9;").as_i32(), Some(4));
    assert_eq!(
        execute_source(
            23,
            "let changed = 0; false && (changed = 1); true || (changed = 2); changed;",
        )
        .as_i32(),
        Some(0)
    );
}

#[test]
fn numeric_negation_and_realm_infinity_preserve_ieee_zero_sign() {
    let value = execute_source(24, "1 / 0 === Infinity && 1 / -0 === -Infinity;");
    assert_eq!(value.as_immediate(), Some(tachyon_value::Immediate::True));
}

#[test]
fn try_catch_preserves_binding_normal_path_and_nested_completion() {
    assert_eq!(
        execute_source(
            25,
            "let result = 0; try { throw 42; } catch (error) { result = error; } result;",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            26,
            "let result = 1; try { result = 7; } catch (error) { result = 9; } result;",
        )
        .as_i32(),
        Some(7)
    );
    assert_eq!(
        execute_source(27, "try { throw 5; } catch { 8; }").as_i32(),
        Some(8)
    );
    assert_eq!(
        execute_source(
            28,
            "let result = 0; try { try { throw 3; } catch (inner) { throw inner; } } catch (outer) { result = outer; } result;",
        )
        .as_i32(),
        Some(3)
    );
}

#[test]
fn callee_throw_enters_caller_catch_without_native_unwind() {
    let value = execute_source(
        29,
        "function fail() { throw 42; } let result = 0; try { fail(); } catch (error) { result = error; } result;",
    );
    assert_eq!(value.as_i32(), Some(42));
}

#[test]
fn function_expressions_are_callable_and_function_objects_hold_methods() {
    assert_eq!(
        execute_source(
            30,
            "let outer = function () { return function () { return 42; }; }; outer()();",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            31,
            "function assert() {} assert._isSameValue = function (value) { return value + 1; }; assert._isSameValue(41);",
        )
        .as_i32(),
        Some(42)
    );
}

#[test]
fn function_prototype_call_forwards_this_and_positional_arguments() {
    let value = execute_source(
        60,
        "function sum(left, right) { return this + left + right; } sum.call(10, 20, 12);",
    );
    assert_eq!(value.as_i32(), Some(42));
}

#[test]
fn function_strictness_controls_nullish_this_binding() {
    let sloppy = execute_source(
        61,
        "function readThis() { return this; } this === readThis.call(undefined);",
    );
    assert_eq!(sloppy.as_immediate(), Some(tachyon_value::Immediate::True));
    let strict = execute_source(
        62,
        "function readThis() { 'use strict'; return this; } readThis.call(undefined) === undefined;",
    );
    assert_eq!(strict.as_immediate(), Some(tachyon_value::Immediate::True));
}

#[test]
fn strict_reference_failures_are_catchable_native_error_objects() {
    let caught = execute_source(
        63,
        "function fail() { 'use strict'; missing = 1; } try { fail(); } catch (error) { error.constructor === ReferenceError; }",
    );
    assert_eq!(caught.as_immediate(), Some(tachyon_value::Immediate::True));
    let constructed = execute_source(
        64,
        "var called = ReferenceError(); var built = new ReferenceError(); called.constructor === ReferenceError && built instanceof ReferenceError;",
    );
    assert_eq!(
        constructed.as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Covers bound this, argument prefixes, nested flattening, and virtual metadata.
fn function_prototype_bind_preserves_call_semantics() {
    assert_eq!(
        execute_source(
            75,
            "function add(first, second) { return this.base + first + second; } let bound = add.bind({ base: 10 }, 20); bound(12) === 42 && bound.length === 1 && bound.name === 'bound add';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            76,
            "function add(first, second, third) { return this.base + first + second + third; } let first = add.bind({ base: 10 }, 1); let second = first.bind({ base: 100 }, 2); second(29) === 42 && second.name === 'bound bound add' && second.length === 1 && second.bind(null, 3).length === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            77,
            "let hasOwn = Function.prototype.call.bind(Object.prototype.hasOwnProperty); hasOwn({ answer: 42 }, 'answer');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Proves bound construction ignores bound-this and delegates HasInstance to its target.
fn bound_function_constructs_with_target_prototype() {
    assert_eq!(
        execute_source(
            78,
            "function Box(first, second) { this.total = first + second; } let Bound = Box.bind({ total: 0 }, 20); let box = new Bound(22); box.total === 42 && box instanceof Box && box instanceof Bound && !Object.hasOwn(Bound, 'prototype');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            79,
            "function defaults(first, second = 2, third) {} defaults.length === 1 && defaults.bind(null, 1).length === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Covers configurable virtual metadata overrides, deletion tombstones, and recreation.
fn function_metadata_properties_follow_ordinary_descriptor_semantics() {
    assert_eq!(
        execute_source(
            80,
            "function pair(first, second) {} Object.defineProperty(pair, 'length', { value: 3.66 }); let first = pair.bind(null); let removed = delete pair.length; let absent = !Object.hasOwn(pair, 'length') && pair.length === 0; pair.length = 7; first.length === 3 && removed && absent && pair.bind(null, 1).length === 6;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            81,
            "function named() {} Object.defineProperty(named, 'name', { value: 'renamed' }); let bound = named.bind(null); let descriptor = Object.getOwnPropertyDescriptor(bound, 'name'); bound.name === 'bound renamed' && descriptor.writable === false && descriptor.enumerable === false && descriptor.configurable === true && !Object.hasOwn(Function.prototype.bind, 'prototype') && Object.hasOwn(Function, 'prototype');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
fn ordinary_construct_sets_receiver_new_target_and_return_replacement() {
    assert_eq!(
        execute_source(
            32,
            "function Box(value) { this.value = value; } (new Box(42)).value;",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            33,
            "function Box() { this.value = 42; return 7; } (new Box()).value;",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            34,
            "function replacement() {} function Box() { return replacement; } new Box() === replacement;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            35,
            "function Box() { return new.target; } let constructed = new Box() === Box; let called = Box() === undefined; constructed && called;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Exercises observable default function prototypes and constructor-selected receiver chains.
fn instanceof_uses_the_current_constructor_prototype_chain() {
    assert_eq!(
        execute_source(
            47,
            "function Constructor() {} Constructor.prototype.constructor === Constructor && new Constructor() instanceof Constructor;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(48, "function Constructor() {} 1 instanceof Constructor;").as_immediate(),
        Some(tachyon_value::Immediate::False)
    );
    assert_eq!(
        execute_source(
            49,
            "function Constructor() {} function Parent() {} Constructor.prototype = Parent.prototype; new Constructor() instanceof Parent;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
fn compound_assignment_reads_old_value_before_rhs_and_evaluates_receiver_once() {
    assert_eq!(
        execute_source(36, "let value = 1; value += (value = 2); value;").as_i32(),
        Some(3)
    );
    assert_eq!(
        execute_source(
            37,
            "function Box() { this.value = 1; this.calls = 0; } function target(receiver) { receiver.calls += 1; return receiver; } let box = new Box(); target(box).value += 2; box.calls === 1 && box.value === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

/// Verifies short-circuit assignment stores only when its logical condition requires it.
#[test]
fn logical_assignment_short_circuits_and_preserves_values() {
    assert_eq!(
        execute_source(54, "let x = 0; x &&= 42; x;").as_i32(),
        Some(0)
    );
    assert_eq!(
        execute_source(55, "let x = 0; x ||= 42; x;").as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(56, "let x = null; x ??= 42; x;").as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(57, "let x = 1; x &&= 42; x;").as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(58, "let box = { value: 0 }; box.value ||= 42; box.value;").as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            59,
            "let box = { value: 1 }; box['value'] &&= 42; box.value;"
        )
        .as_i32(),
        Some(42)
    );
}

#[test]
fn closure_environment_preserves_mutable_state_across_calls() {
    assert_eq!(
        execute_source(
            51,
            "function outer() { let value = 1; return function() { value += 1; return value; }; } let next = outer(); next(); next();",
        )
        .as_i32(),
        Some(3)
    );
    assert_eq!(
        execute_source(
            52,
            "function outer() { let first = 20; return function() { let second = 22; return function() { return first + second; }; }; } outer()()();",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            53,
            "function outer() { let first = 20; function middle() { let second = 22; function inner() { return first + second; } return inner; } return middle; } outer()()();",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            54,
            "function outer() { return inner(); function inner() { return 42; } } outer();",
        )
        .as_i32(),
        Some(42)
    );
}
