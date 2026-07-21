use tachyon_value::Immediate;

use super::execute_source;

#[test]
/// Covers base receiver initialization, default construction, and standard prototype wiring.
fn base_class_construction_and_prototype_wiring() {
    for (source_id, source) in [
        (
            1_069,
            "class A { constructor(a, b) { this.sum = a + b; } } var value = new A(2, 3); value.sum === 5 && value instanceof A;",
        ),
        (
            1_070,
            "class A {} var value = new A(); value instanceof A && Object.getPrototypeOf(A) === Function.prototype && Object.getPrototypeOf(A.prototype) === Object.prototype && A.prototype.constructor === A;",
        ),
        (
            1_071,
            "class A {} var descriptor = Object.getOwnPropertyDescriptor(A, 'prototype'); descriptor.writable === false && descriptor.enumerable === false && descriptor.configurable === false;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(Immediate::True),
            "failed source: {source}",
        );
    }
}

#[test]
/// Enforces class-only call behavior and the base constructor receiver-return rules.
fn base_class_call_and_return_semantics() {
    for (source_id, source) in [
        (
            1_072,
            "class A {} var threw = false; try { A(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_073,
            "class A { constructor() { return 1; } } new A() instanceof A;",
        ),
        (
            1_074,
            "var replacement = {}; class A { constructor() { return replacement; } } new A() === replacement;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(Immediate::True),
            "failed source: {source}",
        );
    }
}
