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

#[test]
/// Preserves the private immutable class name through heritage, methods, and outer shadowing.
fn named_class_expression_environment() {
    for (source_id, source) in [
        (
            1_082,
            "var value = class Hidden { method() { return 1; } }; value.name === 'Hidden' && value.prototype.method() === 1 && typeof Hidden === 'undefined';",
        ),
        (
            1_083,
            "var value = class Hidden { static self() { return Hidden; } }; value.self() === value && typeof Hidden === 'undefined';",
        ),
        (
            1_084,
            "var Hidden = 7; var value = class Hidden { static self() { return Hidden; } }; value.self() === value && Hidden === 7;",
        ),
        (
            1_085,
            "var threw = false; try { var value = class Hidden extends Hidden {}; } catch (error) { threw = error instanceof ReferenceError; } threw && typeof Hidden === 'undefined';",
        ),
        (
            1_086,
            "var value = class Hidden { constructor() { this.owner = Hidden; } method() { let captured = 1; return function() { return captured === 1 && Hidden; }; } }; var instance = new value(); instance.owner === value && instance.method()() === value;",
        ),
        (
            1_087,
            "var Outer = class OuterName { static make() { return class Inner { static self() { return Inner; } static outer() { return OuterName; } }; } }; var Inner = Outer.make(); Inner.self() === Inner && Inner.outer() === Outer;",
        ),
        (
            1_088,
            "function run() { let captured = 7; function read() { return captured; } try { var value = class Hidden extends Hidden {}; } catch (error) {} return read(); } run() === 7;",
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
/// Delays static field initializers until all keys exist while preserving class execution context.
fn public_static_field_semantics() {
    for (source_id, source) in [
        (
            1_089,
            "var index = 0; function next() { var key = 'k' + index; index = index + 1; return key; } class A { static [next()] = index; static [next()] = index; } index === 2 && A.k0 === 2 && A.k1 === 2;",
        ),
        (
            1_090,
            "class Base {} Base.base = 4; class Derived extends Base { static self = this; static value = super.base + 1; static owner = Derived; } Derived.self === Derived && Derived.value === 5 && Derived.owner === Derived;",
        ),
        (
            1_091,
            "class A { static field = 7; static empty; static named = function() {}; } var descriptor = Object.getOwnPropertyDescriptor(A, 'field'); A.field === 7 && A.empty === undefined && A.named.name === 'named' && descriptor.writable === true && descriptor.enumerable === true && descriptor.configurable === true;",
        ),
        (
            1_092,
            "var threw = false; try { class C { [typeof C]() {} } } catch (error) { threw = error instanceof ReferenceError; } threw;",
        ),
        (
            1_093,
            "var saved; function outer(parameter) { let lexical = 1; var variable = 2; class C { static value = parameter + lexical + variable; static self = C; method() { return C; } } saved = function() { return C; }; return C; } var C = outer(3); C.value === 6 && C.self === C && new C().method() === C && saved() === C;",
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
/// Initializes public instance fields at the base/derived specification points with own data slots.
fn public_instance_field_semantics() {
    for (source_id, source) in [
        (
            1_094,
            "var order = ''; class C { field = (order = order + 'f', 1); constructor(value = (order = order + 'p', 2)) { order = order + 'b'; this.value = value; } } var instance = new C(); var descriptor = Object.getOwnPropertyDescriptor(instance, 'field'); order === 'fpb' && instance.field === 1 && instance.value === 2 && descriptor.writable === true && descriptor.enumerable === true && descriptor.configurable === true;",
        ),
        (
            1_095,
            "class Base { set field(value) { throw 1; } } Base.prototype.source = 4; class Derived extends Base { field = 7; fromSuper = super.source + 1; named = function() {}; constructor() { super(); } } var instance = new Derived(); instance.field === 7 && instance.fromSuper === 5 && instance.named.name === 'named';",
        ),
        (
            1_096,
            "var keys = 0; function key() { keys = keys + 1; return 'field'; } class C { [key()] = keys; } var first = new C(); var second = new C(); keys === 1 && first.field === 1 && second.field === 1;",
        ),
        (
            1_097,
            "var definitions = 0; class Base { constructor() { return new Proxy({}, { defineProperty(target, key, descriptor) { definitions = definitions + 1; return Reflect.defineProperty(target, key, descriptor); } }); } } class Derived extends Base { field = 7; } var instance = new Derived(); definitions === 1 && instance.field === 7;",
        ),
        (
            1_098,
            "class Base { constructor() { return Object.preventExtensions({}); } } class Derived extends Base { field = 1; } var threw = false; try { new Derived(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_099,
            "var order = ''; function fail() { order = order + 'b'; throw 9; } class Base {} class Derived extends Base { a = (order = order + 'a', 1); b = fail(); c = (order = order + 'c', 3); constructor() { try { super(); } catch (error) { this.caught = error === 9 && this.a === 1 && !Object.hasOwn(this, 'b'); } } } var instance = new Derived(); order === 'ab' && instance.caught === true && !Object.hasOwn(instance, 'c');",
        ),
        (
            1_100,
            "var baseRuns = 0; var fieldRuns = 0; class Base { constructor() { baseRuns = baseRuns + 1; } } class Derived extends Base { field = (fieldRuns = fieldRuns + 1, 1); constructor() { super(); try { super(); } catch (error) { this.second = error instanceof ReferenceError; } } } var instance = new Derived(); baseRuns === 2 && fieldRuns === 1 && instance.field === 1 && instance.second === true;",
        ),
        (
            1_101,
            "var fieldRuns = 0; class Base {} class Derived extends Base { field = (fieldRuns = fieldRuns + 1, 1); constructor() { return {}; } } var instance = new Derived(); fieldRuns === 0 && !Object.hasOwn(instance, 'field');",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(Immediate::True),
            "failed source: {source}",
        );
    }
}
