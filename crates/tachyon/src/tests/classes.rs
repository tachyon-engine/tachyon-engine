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

#[test]
/// Publishes paired instance/static accessors with class names and descriptor attributes.
fn class_accessor_semantics_and_descriptors() {
    let source = "class A { get value() { return this._value; } set value(next) { this._value = next; } static get answer() { return 42; } } var instance = new A(); instance.value = 7; var descriptor = Object.getOwnPropertyDescriptor(A.prototype, 'value'); var staticDescriptor = Object.getOwnPropertyDescriptor(A, 'answer'); instance.value === 7 && A.answer === 42 && descriptor.get.name === 'get value' && descriptor.set.name === 'set value' && descriptor.enumerable === false && descriptor.configurable === true && staticDescriptor.get.name === 'get answer' && staticDescriptor.enumerable === false;";
    assert_eq!(
        execute_source(1_075, source).as_immediate(),
        Some(Immediate::True),
    );
}

#[test]
/// Preserves source order, inferred names, Symbol spelling, and class descriptors for computed keys.
fn computed_class_element_semantics() {
    for (source_id, source) in [
        (
            1_076,
            "var order = ''; function key(name) { order = order + name; return name; } class A { [key('a')]() { return 1; } static [key('b')]() { return 2; } get [key('c')]() { return this._c; } set [key('c')](value) { this._c = value; } } var instance = new A(); instance.c = 3; var descriptor = Object.getOwnPropertyDescriptor(A.prototype, 'c'); order === 'abcc' && instance.a() === 1 && A.b() === 2 && instance.c === 3 && A.prototype.a.name === 'a' && A.b.name === 'b' && descriptor.get.name === 'get c' && descriptor.set.name === 'set c' && descriptor.enumerable === false;",
        ),
        (
            1_077,
            "var key = Symbol('method'); class A { [key]() { return 1; } } A.prototype[key].name === '[method]' && A.prototype[key]() === 1;",
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
/// Uses dynamic HomeObject prototypes while preserving the call-site `this` receiver.
fn class_super_property_semantics() {
    for (source_id, source) in [
        (
            1_078,
            "class A { value() { return this.x + 1; } get current() { return this.x; } static value() { return this.x + 1; } } class B extends A { value() { return super.value() + 1; } get current() { return super.current + 1; } static value() { return super.value() + 1; } computed(key) { return super[key](); } } B.x = 3; var instance = new B(); instance.x = 4; instance.value() === 6 && instance.current === 5 && B.value() === 5 && instance.computed('value') === 5;",
        ),
        (
            1_079,
            "class A { value() { return this.x; } } class B extends A { value() { return super.value(); } } var method = B.prototype.value; var custom = { x: 9 }; method.call(custom) === 9;",
        ),
        (
            1_080,
            "class A { value() { return 1; } } class B extends A { value() { return super.value(); } } class Other { value() { return 7; } } Object.setPrototypeOf(B.prototype, Other.prototype); new B().value() === 7;",
        ),
        (
            1_081,
            "class A { value() { return 4; } } class B extends A { value() { return super[(() => { Object.setPrototypeOf(B.prototype, null); return 'value'; })()](); } } new B().value() === 4;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(Immediate::True),
            "failed source: {source}",
        );
    }
}
