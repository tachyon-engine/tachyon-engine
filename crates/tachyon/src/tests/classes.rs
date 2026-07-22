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

#[test]
/// Executes static blocks after key collection while preserving source order and lexical boundaries.
fn class_static_block_semantics() {
    for (source_id, source) in [
        (
            1_102,
            "var order = ''; function key() { order = order + 'k'; return 'method'; } class Base {} Base.inherited = 4; class C extends Base { static first = (order = order + 'f', 1); [key()]() {} static { order = order + 'b'; this.fromSuper = super.inherited + 1; } static last = (order = order + 'l', 2); } order === 'kfbl' && C.first === 1 && C.last === 2 && C.fromSuper === 5;",
        ),
        (
            1_103,
            "class C { static { this.first = helper(); function helper() { return 7; } let local = 3; this.read = function() { return local; }; this.target = new.target; } static { let local = 5; this.second = local; } } C.first === 7 && C.read() === 3 && C.second === 5 && C.target === undefined;",
        ),
        (
            1_104,
            "function make(seed) { return class Named { static { this.value = seed + 1; this.self = Named; } }; } var C = make(6); C.value === 7 && C.self === C;",
        ),
        (
            1_105,
            "var order = ''; var caught = false; try { class C { static { order = order + 'a'; throw 9; } static field = (order = order + 'b', 1); } } catch (error) { caught = error === 9; } caught && order === 'a';",
        ),
        (
            1_106,
            "class Base {} Base.value = 1; class Other {} Other.value = 8; class C extends Base { static { Object.setPrototypeOf(this, Other); this.read = super.value; } } C.read === 8;",
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
/// Initializes static private elements on the defining constructor in specification order.
fn static_private_element_semantics() {
    for (source_id, source) in [
        (
            1_137,
            "var order = ''; class C { static #first = (order += 'a', 1); static { order += 'b'; this.middle = this.#first; } static #second = (order += 'c', 2); static read() { return this.#first + this.#second; } } order === 'abc' && C.middle === 1 && C.read() === 3 && !Object.hasOwn(C, '#first');",
        ),
        (
            1_138,
            "class C { static #value = this.#method(); static #method() { return 4; } static read() { return this.#value; } static method() { return this.#method; } } C.read() === 4 && C.method() === C.method() && C.method().name === '#method';",
        ),
        (
            1_139,
            "class Base { static value(input) { return input + 1; } } class C extends Base { static #stored = 1; static get #value() { return super.value(this.#stored); } static set #value(next) { this.#stored = super.value(next); } static read() { return this.#value; } static write(next) { return this.#value = next; } static stored() { return this.#stored; } } C.read() === 2 && C.write(6) === 6 && C.stored() === 7;",
        ),
        (
            1_140,
            "class C { static #value = 1; static #method() { return 2; } static get #accessor() { return 3; } static readValue() { return this.#value; } static readMethod() { return this.#method(); } static readAccessor() { return this.#accessor; } } class D extends C {} var proxy = new Proxy(C, {}); var instance = new C(); function rejects(receiver, method) { try { method.call(receiver); } catch (error) { return error instanceof TypeError; } return false; } rejects(D, C.readValue) && rejects(proxy, C.readMethod) && rejects(instance, C.readAccessor);",
        ),
        (
            1_141,
            "class C { static #method() { return 1; } static overwrite() { this.#method = 2; } static update() { this.#method++; } } var assignment = false; var update = false; try { C.overwrite(); } catch (error) { assignment = error instanceof TypeError; } try { C.update(); } catch (error) { update = error instanceof TypeError; } assignment && update;",
        ),
        (
            1_142,
            "class Outer { static #value = 1; static make() { return class Inner { static #value = 2; static read(receiver) { return receiver.#value; } }; } static read() { return this.#value; } } var Inner = Outer.make(); var wrong = false; try { Inner.read(Outer); } catch (error) { wrong = error instanceof TypeError; } Outer.read() === 1 && Inner.read(Inner) === 2 && wrong;",
        ),
        (
            1_143,
            "class C { static before() { return 1; } static #value; static write(next) { this.#value = next; return this.#value; } } C.before() === 1 && C.write(4) === 4;",
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
/// Checks private brands without invoking accessors, prototypes, or Proxy traps.
fn private_brand_check_semantics() {
    for (source_id, source) in [
        (
            1_144,
            "var calls = 0; class C { #field; #method() { calls++; } get #accessor() { calls++; } static field(value) { return #field in value; } static method(value) { return #method in value; } static accessor(value) { return #accessor in value; } } var value = new C(); C.field(value) && C.method(value) && C.accessor(value) && !C.field({}) && calls === 0;",
        ),
        (
            1_145,
            "class Outer { #value; static has(value) { return #value in value; } static make() { return class Inner { #value; static has(value) { return #value in value; } }; } } var Inner = Outer.make(); var outer = new Outer(); var inner = new Inner(); Outer.has(outer) && !Outer.has(inner) && Inner.has(inner) && !Inner.has(outer);",
        ),
        (
            1_146,
            "class C { #value; static has(value) { return #value in value; } } var threw = false; try { C.has(1); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_147,
            "var calls = 0; class C { #value; static has() { return #value in (calls++, new C()); } } C.has() && calls === 1;",
        ),
        (
            1_148,
            "var traps = 0; class Base { constructor() { return new Proxy({}, { has() { traps++; return false; } }); } } class C extends Base { #value; static has(value) { return #value in value; } constructor() { super(); } } var stamped = new C(); var plain = new Proxy({}, { has() { traps++; return true; } }); C.has(stamped) && !C.has(plain) && traps === 0;",
        ),
        (
            1_149,
            "class C { static #value; static has(value) { return #value in value; } } class D extends C {} C.has(C) && !C.has(D) && !C.has(new C()) && !C.has(new Proxy(C, {}));",
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
/// Keeps instance private data unforgeable while preserving field evaluation and update semantics.
fn private_instance_field_semantics() {
    for (source_id, source) in [
        (
            1_107,
            "class C { #value; read() { return this.#value; } write(value) { return this.#value = value; } } var value = new C(); value.read() === undefined && value.write(4) === 4 && value.read() === 4 && Reflect.ownKeys(value).length === 0;",
        ),
        (
            1_108,
            "var order = ''; class C { first = (order = order + 'a', 1); #second = (order = order + 'b', 2); third = (order = order + 'c', 3); read() { return this.#second; } } var value = new C(); order === 'abc' && value.first === 1 && value.read() === 2 && value.third === 3;",
        ),
        (
            1_109,
            "class C { #value = 1; update() { var a = this.#value++; var b = ++this.#value; this.#value += 4; return a === 1 && b === 3 && this.#value === 7; } } new C().update();",
        ),
        (
            1_110,
            "class C { #value = 1; method() { return function() { return this.#value; }; } } var value = new C(); value.method().call(value) === 1;",
        ),
        (
            1_111,
            "class Outer { #value = 1; make() { return class Inner { #value = 2; read(value) { return value.#value; } }; } } var outer = new Outer(); var Inner = outer.make(); var inner = new Inner(); var threw = false; try { inner.read(outer); } catch (error) { threw = error instanceof TypeError; } inner.read(inner) === 2 && threw;",
        ),
        (
            1_114,
            "class Outer { #outer = 1; make() { return class Inner { #inner = 2; read(value) { return value.#outer + this.#inner; } }; } } var outer = new Outer(); var Inner = outer.make(); new Inner().read(outer) === 3;",
        ),
        (
            1_112,
            "class C { #value = 1; read() { return this.#value; } } var read = C.prototype.read; var threw = false; try { read.call({}); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_113,
            "class Base { constructor() { return Object.preventExtensions({}); } } class Derived extends Base { #value = 3; constructor() { super(); } } var threw = false; try { new Derived(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_115,
            "var traps = 0; class Base { constructor() { return new Proxy({}, { get() { traps = traps + 1; }, set() { traps = traps + 1; }, defineProperty() { traps = traps + 1; } }); } } class Derived extends Base { #value = 3; read() { return this.#value; } write(value) { this.#value = value; } update() { return ++this.#value; } constructor() { super(); } } var value = new Derived(); var first = Derived.prototype.read.call(value); Derived.prototype.write.call(value, 4); var next = Derived.prototype.update.call(value); first === 3 && next === 5 && Derived.prototype.read.call(value) === 5 && traps === 0;",
        ),
        (
            1_116,
            "class C { first = this.#method(); #method() { return 42; } second = this.#method(); read() { return this.#method; } } var first = new C(); var second = new C(); first.first === 42 && first.second === 42 && first.read() === second.read() && first.read().name === '#method';",
        ),
        (
            1_117,
            "class Base { value() { return 4; } } class Derived extends Base { #method() { return super.value() + 1; } call() { return this.#method(); } } new Derived().call() === 5;",
        ),
        (
            1_118,
            "class C { #method() { return 1; } read() { return this.#method; } } var threw = false; try { C.prototype.read.call({}); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_119,
            "class C { #first = 1; #second = 2; read() { return this.#first + this.#second; } } new C().read() === 3;",
        ),
        (
            1_120,
            "class C { #first() { return 1; } #second() { return 2; } read() { return this.#first() + this.#second(); } } new C().read() === 3;",
        ),
        (
            1_121,
            "class C { #value = 2; #method() {} readValue() { return this.#value; } readMethod() { return this.#method; } } var value = new C(); value.readValue() === 2 && typeof value.readMethod() === 'function';",
        ),
        (
            1_122,
            "class C { #value = 2; #method() { return 1; } read() { return this.#method(); } } new C().read() === 1;",
        ),
        (
            1_123,
            "class C { #value = 2; #method() { return this.#value; } read() { return this.#method(); } } new C().read() === 2;",
        ),
        (
            1_124,
            "class Base { constructor() { return new Proxy({}, {}); } } class C extends Base { #method() { return 1; } read() { return this.#method(); } constructor() { super(); } } var value = new C(); C.prototype.read.call(value) === 1;",
        ),
        (
            1_125,
            "class Base { constructor() { return new Proxy({}, {}); } } class C extends Base { #value = 2; #method() { return this.#value; } read() { return this.#method(); } constructor() { super(); } } var value = new C(); C.prototype.read.call(value) === 2;",
        ),
        (
            1_126,
            "class Base { constructor() { return new Proxy({}, {}); } } class C extends Base { #value = 2; #first = this.#method(); #method() { return this.#value; } read() { return this.#first; } constructor() { super(); } } var value = new C(); C.prototype.read.call(value) === 2;",
        ),
        (
            1_127,
            "class Base { constructor() { return Object.preventExtensions({}); } } class C extends Base { #method() { return 1; } constructor() { super(); } } var threw = false; try { new C(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_128,
            "class C { #method() { return 1; } overwrite() { this.#method = 2; } update() { this.#method++; } } var value = new C(); var assignment = false; var update = false; try { value.overwrite(); } catch (error) { assignment = error instanceof TypeError; } try { value.update(); } catch (error) { update = error instanceof TypeError; } assignment && update;",
        ),
        (
            1_129,
            "class C { #value = 1; get #accessor() { return this.#value + 1; } set #accessor(next) { this.#value = next - 1; } read() { return this.#accessor; } write(next) { return this.#accessor = next; } value() { return this.#value; } } var value = new C(); value.read() === 2 && value.write(8) === 8 && value.value() === 7;",
        ),
        (
            1_130,
            "class C { get #readOnly() { return 1; } set #writeOnly(value) {} readMissing() { return this.#writeOnly; } writeMissing() { this.#readOnly = 2; } } var value = new C(); var read = false; var write = false; try { value.readMissing(); } catch (error) { read = error instanceof TypeError; } try { value.writeMissing(); } catch (error) { write = error instanceof TypeError; } read && write;",
        ),
        (
            1_131,
            "class Base { value() { return 4; } } class C extends Base { get #value() { return super.value() + 1; } read() { return this.#value; } } new C().read() === 5;",
        ),
        (
            1_132,
            "var traps = 0; class Base { constructor() { return new Proxy({}, { get() { traps++; }, set() { traps++; }, defineProperty() { traps++; } }); } } class C extends Base { #value = 2; get #accessor() { return this.#value; } set #accessor(next) { this.#value = next; } read() { return this.#accessor; } write(next) { this.#accessor = next; } constructor() { super(); } } var value = new C(); var first = C.prototype.read.call(value); C.prototype.write.call(value, 4); first === 2 && C.prototype.read.call(value) === 4 && traps === 0;",
        ),
        (
            1_133,
            "class Base { constructor() { return Object.preventExtensions({}); } } class C extends Base { get #accessor() { return 1; } constructor() { super(); } } var threw = false; try { new C(); } catch (error) { threw = error instanceof TypeError; } threw;",
        ),
        (
            1_134,
            "class C { get #value() { throw 1; } set #value(next) { throw next; } read() { return this.#value; } write() { this.#value = 2; } } var value = new C(); var read = 0; var write = 0; try { value.read(); } catch (error) { read = error; } try { value.write(); } catch (error) { write = error; } read === 1 && write === 2;",
        ),
        (
            1_135,
            "var log = ''; class C { #stored = 1; get #value() { log += 'g'; return this.#stored; } set #value(next) { log += 's'; this.#stored = next; } add() { this.#value += (log += 'r', 2); } update() { return this.#value++; } read() { return this.#stored; } } var value = new C(); value.add(); var old = value.update(); log === 'grsgs' && old === 3 && value.read() === 4;",
        ),
        (
            1_136,
            "class C { #a; #b; #c; get #readA() { return this.#a; } get #readB() { return this.#b; } get #readC() { return this.#c; } a(value) { this.#a = value; return this.#readA; } b(value) { this.#b = value; return this.#readB; } c(value) { this.#c = value; return this.#readC; } } var value = new C(); value.a(1) === 1 && value.b(2) === 2 && value.c(3) === 3;",
        ),
    ] {
        assert_eq!(
            execute_source(source_id, source).as_immediate(),
            Some(Immediate::True),
            "failed source: {source}",
        );
    }
}
