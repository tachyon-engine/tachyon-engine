use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const CLASS_PROMISE_SOURCE: &str = r#"
var createBadPromise = false;
var object = {};
class P extends Promise {
  constructor(executor) {
    if (createBadPromise) {
      executor(
        function(value) { if (value !== object) throw 91; },
        function() { throw 92; }
      );
      return object;
    }
    return super(executor);
  }
}
var promise = P.resolve(object);
createBadPromise = true;
var result = promise.then();
createBadPromise = false;
result === object;
"#;

const DEFAULT_DERIVED_SOURCE: &str = r#"
function Base(a, b) { this.sum = a + b; }
class P extends Base {}
var value = new P(2, 3);
value.sum === 5 && value instanceof P && value instanceof Base;
"#;

const BASE_CLASS_SOURCE: &str = r#"
class A {
  constructor(a, b) { this.sum = a + b; }
  value() { return this.sum; }
}
var instance = new A(2, 3);
instance.value() === 5 && instance instanceof A;
"#;

const CLASS_ACCESSOR_SOURCE: &str = r#"
class A {
  get value() { return this._value; }
  set value(next) { this._value = next; }
  static get answer() { return 42; }
}
var instance = new A();
instance.value = 7;
instance.value === 7 && A.answer === 42;
"#;

const COMPUTED_CLASS_SOURCE: &str = r#"
var order = "";
function key(name) { order = order + name; return name; }
class A {
  [key("a")]() { return 1; }
  static [key("b")]() { return 2; }
  get [key("c")]() { return this._c; }
  set [key("c")](value) { this._c = value; }
}
var instance = new A();
instance.c = 3;
order === "abcc" && instance.a() === 1 && A.b() === 2 && instance.c === 3;
"#;

const SUPER_PROPERTY_SOURCE: &str = r#"
class A {
  value() { return this.x + 1; }
  get current() { return this.x; }
  static value() { return this.x + 1; }
}
class B extends A {
  value() { return super.value() + 1; }
  get current() { return super.current + 1; }
  static value() { return super.value() + 1; }
  computed(key) { return super[key](); }
}
B.x = 3;
var instance = new B();
instance.x = 4;
instance.value() === 6 && instance.current === 5 && B.value() === 5 && instance.computed("value") === 5;
"#;

const SUPER_PROPERTY_CONSTRUCTOR_SOURCE: &str = r#"
class A { value() { return this.x; } }
class B extends A {
  constructor() {
    super();
    this.x = 4;
    this.y = super.value();
  }
}
var instance = new B();
instance.y === 4;
"#;

const NAMED_CLASS_ENVIRONMENT_SOURCE: &str = r#"
var Outer = 7;
var value = class Inner {
  static self() { return Inner; }
  method() { return Inner; }
};
var instance = new value();
value.self() === value && instance.method() === value && Outer === 7 && typeof Inner === "undefined";
"#;

const STATIC_FIELD_SOURCE: &str = r#"
var index = 0;
function next() { var key = "k" + index; index = index + 1; return key; }
class Base { static base = 4; }
class Derived extends Base {
  static [next()] = index;
  static self = this;
  static value = super.base + index;
  static [next()] = index;
}
var saved;
function outer(parameter) {
  let lexical = 1;
  var variable = 2;
  class Captured { static value = parameter + lexical + variable; static self = Captured; }
  saved = function() { return Captured; };
  return Captured;
}
var Captured = outer(3);
index === 2 && Derived.k0 === 2 && Derived.k1 === 2 && Derived.self === Derived && Derived.value === 6 && Captured.value === 6 && Captured.self === Captured && saved() === Captured;
"#;

const INSTANCE_FIELD_SOURCE: &str = r#"
var definitions = 0;
var symbol = Symbol("field");
function makeClass(seed) {
  let captured = seed + 1;
  class Base {
    constructor() {
      return new Proxy({}, {
        defineProperty(target, key, descriptor) {
          definitions = definitions + 1;
          return Reflect.defineProperty(target, key, descriptor);
        }
      });
    }
  }
  return class Derived extends Base {
    first = captured;
    [symbol] = super.missing;
    named = function() {};
  };
}
var Derived = makeClass(6);
var value = new Derived();
definitions === 3 && value.first === 7 && value[symbol] === undefined && value.named.name === "named";
"#;

const STATIC_BLOCK_SOURCE: &str = r#"
var order = "";
function make(seed) {
  return class Named {
    static first = (order = order + "f", seed);
    static {
      order = order + "b";
      let local = seed + 1;
      this.read = function() { return local; };
      this.self = Named;
    }
    static last = (order = order + "l", seed + 2);
  };
}
var C = make(5);
order === "fbl" && C.first === 5 && C.read() === 6 && C.self === C && C.last === 7;
"#;

#[test]
fn derived_class_promise_trampoline_works_for_every_dispatch_batch() {
    assert_class_promise_batch::<1>();
    assert_class_promise_batch::<2>();
    assert_class_promise_batch::<4>();
    assert_class_promise_batch::<8>();
    assert_class_promise_batch::<16>();
}

#[test]
fn default_derived_constructor_forwards_for_every_dispatch_batch() {
    assert_default_derived_batch::<1>();
    assert_default_derived_batch::<2>();
    assert_default_derived_batch::<4>();
    assert_default_derived_batch::<8>();
    assert_default_derived_batch::<16>();
}

#[test]
fn base_class_constructs_for_every_dispatch_batch() {
    assert_base_class_batch::<1>();
    assert_base_class_batch::<2>();
    assert_base_class_batch::<4>();
    assert_base_class_batch::<8>();
    assert_base_class_batch::<16>();
}

#[test]
fn class_accessors_execute_for_every_dispatch_batch() {
    assert_class_accessor_batch::<1>();
    assert_class_accessor_batch::<2>();
    assert_class_accessor_batch::<4>();
    assert_class_accessor_batch::<8>();
    assert_class_accessor_batch::<16>();
}

#[test]
fn computed_class_elements_execute_for_every_dispatch_batch() {
    assert_computed_class_batch::<1>();
    assert_computed_class_batch::<2>();
    assert_computed_class_batch::<4>();
    assert_computed_class_batch::<8>();
    assert_computed_class_batch::<16>();
}

#[test]
fn super_properties_execute_for_every_dispatch_batch() {
    assert_super_property_batch::<1>();
    assert_super_property_batch::<2>();
    assert_super_property_batch::<4>();
    assert_super_property_batch::<8>();
    assert_super_property_batch::<16>();
}

#[test]
fn named_class_environments_execute_for_every_dispatch_batch() {
    assert_named_class_environment_batch::<1>();
    assert_named_class_environment_batch::<2>();
    assert_named_class_environment_batch::<4>();
    assert_named_class_environment_batch::<8>();
    assert_named_class_environment_batch::<16>();
}

#[test]
fn static_fields_execute_for_every_dispatch_batch() {
    assert_static_field_batch::<1>();
    assert_static_field_batch::<2>();
    assert_static_field_batch::<4>();
    assert_static_field_batch::<8>();
    assert_static_field_batch::<16>();
}

#[test]
fn static_fields_survive_forced_major_collections() {
    assert_forced_major_source(STATIC_FIELD_SOURCE, 49);
}

#[test]
fn instance_fields_execute_for_every_dispatch_batch() {
    assert_instance_field_batch::<1>();
    assert_instance_field_batch::<2>();
    assert_instance_field_batch::<4>();
    assert_instance_field_batch::<8>();
    assert_instance_field_batch::<16>();
}

#[test]
fn instance_field_plans_survive_forced_major_collections() {
    for (source, source_id) in [
        ("class C { field = 1; } true;", 150),
        ("class C { field = 1; } var value = new C(); true;", 154),
        ("class C { field = 1; } new C().field === 1;", 155),
        (
            "var key = Symbol('field'); class C { [key] = 1; } new C()[key] === 1;",
            151,
        ),
        (
            "function make(seed) { let captured = seed + 1; return class { field = captured; }; } var C = make(6); new C().field === 7;",
            152,
        ),
        (
            "var count = 0; class Base { constructor() { return new Proxy({}, { defineProperty(target, key, descriptor) { count = count + 1; return Reflect.defineProperty(target, key, descriptor); } }); } } class C extends Base { field = 1; } var value = new C(); count === 1 && value.field === 1;",
            156,
        ),
        (
            "var key = Symbol('field'); class Base { constructor() { return new Proxy({}, { defineProperty(target, key, descriptor) { return Reflect.defineProperty(target, key, descriptor); } }); } } class C extends Base { first = 1; [key] = 2; third = 3; } var value = new C(); value.first === 1 && value[key] === 2 && value.third === 3;",
            157,
        ),
        (
            "class Base { constructor() { return new Proxy({}, { defineProperty(target, key, descriptor) { return Reflect.defineProperty(target, key, descriptor); } }); } } Base.prototype.value = 4; class C extends Base { field = super.value; } new C().field === 4;",
            158,
        ),
        (
            "function make(seed) { let captured = seed + 1; class Base { constructor() { return new Proxy({}, { defineProperty(target, key, descriptor) { return Reflect.defineProperty(target, key, descriptor); } }); } } return class extends Base { field = captured; }; } var C = make(6); new C().field === 7;",
            159,
        ),
        (
            "class Base { constructor() { return new Proxy({}, { defineProperty(target, key, descriptor) { return Reflect.defineProperty(target, key, descriptor); } }); } } class C extends Base { named = function() {}; } new C().named.name === 'named';",
            160,
        ),
        (INSTANCE_FIELD_SOURCE, 153),
    ] {
        assert_forced_major_source(source, source_id);
    }
}

#[test]
fn static_blocks_execute_for_every_dispatch_batch() {
    assert_static_block_batch::<1>();
    assert_static_block_batch::<2>();
    assert_static_block_batch::<4>();
    assert_static_block_batch::<8>();
    assert_static_block_batch::<16>();
}

#[test]
fn static_blocks_survive_forced_major_collections() {
    assert_forced_major_source(STATIC_BLOCK_SOURCE, 166);
}

#[test]
fn derived_class_promise_state_survives_forced_major_collections() {
    assert_forced_major_source(CLASS_PROMISE_SOURCE, 32);
}

#[test]
fn derived_class_creation_survives_forced_major_collections() {
    assert_forced_major_source(
        "class P extends Promise { constructor(executor) { return super(executor); } } true;",
        33,
    );
}

#[test]
fn derived_class_static_resolve_survives_forced_major_collections() {
    assert_forced_major_source(
        "class P extends Promise { constructor(executor) { return super(executor); } } P.resolve(1); true;",
        34,
    );
}

#[test]
fn derived_class_static_reject_survives_forced_major_collections() {
    assert_forced_major_source(
        "class P extends Promise { constructor(executor) { return super(executor); } } P.reject(1); true;",
        35,
    );
}

#[test]
fn derived_class_methods_survive_forced_major_collections() {
    assert_forced_major_source(
        "class P extends Promise { constructor(executor) { super(executor); } value() { return 7; } static make(executor) { return new this(executor); } } var value = P.make(function() {}); value.value() === 7;",
        36,
    );
}

#[test]
fn default_derived_promise_constructor_survives_forced_major_collections() {
    assert_forced_major_source(
        "class P extends Promise {} var value = P.resolve(1); value instanceof P && value instanceof Promise;",
        37,
    );
}

#[test]
fn base_class_creation_survives_forced_major_collections() {
    assert_forced_major_source(BASE_CLASS_SOURCE, 38);
}

#[test]
fn class_accessors_survive_forced_major_collections() {
    assert_forced_major_source(CLASS_ACCESSOR_SOURCE, 39);
}

#[test]
fn computed_class_elements_survive_forced_major_collections() {
    for (source, source_id) in [
        ("class A { ['a']() { return 1; } } true;", 40),
        (
            "function key(value) { return value; } class A { [key('a')]() { return 1; } } true;",
            41,
        ),
        (
            "class A { get ['a']() { return 1; } set ['a'](value) {} } true;",
            42,
        ),
        (COMPUTED_CLASS_SOURCE, 43),
    ] {
        assert_forced_major_source(source, source_id);
    }
}

#[test]
fn super_property_home_objects_survive_forced_major_collections() {
    assert_forced_major_source(SUPER_PROPERTY_SOURCE, 44);
    assert_forced_major_source(SUPER_PROPERTY_CONSTRUCTOR_SOURCE, 45);
}

#[test]
fn named_class_environments_survive_forced_major_collections() {
    assert_forced_major_source(NAMED_CLASS_ENVIRONMENT_SOURCE, 46);
    assert_forced_major_source(
        "var value = class Inner { method() { let captured = 1; return function() { return captured === 1 && Inner; }; } }; var closure = new value().method(); closure() === value;",
        47,
    );
    assert_forced_major_source(
        "var threw = false; try { var value = class Inner extends Inner {}; } catch (error) { threw = error instanceof ReferenceError; } var object = {}; threw && object !== null;",
        48,
    );
}

/// Executes a focused class fixture with collection before every managed allocation.
fn assert_forced_major_source(source: &str, source_id: u32) {
    let module = compile_source(source, source_id);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 2_048,
                quantum: 2_048,
            },
        )
        .unwrap_or_else(|error| panic!("forced-major class fixture failed: {error:?}; {source}"));
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major class fixture returned {outcome:?}; {source}"
    );
}

/// Compiles once per monomorphization and requires the complete checkpoint to stay successful.
fn assert_class_promise_batch<const N: usize>() {
    let module = compile_class_promise_fixture(N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 2_048,
                quantum: 2_048,
            },
        )
        .expect("class fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes the synthetic forwarding constructor with each tuned dispatch batch.
fn assert_default_derived_batch<const N: usize>() {
    let module = compile_source(DEFAULT_DERIVED_SOURCE, 40 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 512,
                quantum: 512,
            },
        )
        .expect("default derived fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes base allocation, body initialization, and method dispatch with each batch size.
fn assert_base_class_batch<const N: usize>() {
    let module = compile_source(BASE_CLASS_SOURCE, 50 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 512,
                quantum: 512,
            },
        )
        .expect("base class fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes getter/setter publication and calls with each tuned dispatch batch.
fn assert_class_accessor_batch<const N: usize>() {
    let module = compile_source(CLASS_ACCESSOR_SOURCE, 70 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 512,
                quantum: 512,
            },
        )
        .expect("class accessor fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes source-ordered computed keys, runtime names, and definitions with each batch size.
fn assert_computed_class_batch<const N: usize>() {
    let module = compile_source(COMPUTED_CLASS_SOURCE, 90 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 768,
                quantum: 768,
            },
        )
        .expect("computed class fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes class HomeObject lookup and receiver-preserving super calls with each batch size.
fn assert_super_property_batch<const N: usize>() {
    let module = compile_source(SUPER_PROPERTY_SOURCE, 110 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 1_024,
                quantum: 1_024,
            },
        )
        .expect("super property fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes private class-name capture and outer-scope restoration with each dispatch batch.
fn assert_named_class_environment_batch<const N: usize>() {
    let module = compile_source(NAMED_CLASS_ENVIRONMENT_SOURCE, 130 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 1_024,
                quantum: 1_024,
            },
        )
        .expect("named class environment fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes delayed static field records with one tuned dispatch batch.
fn assert_static_field_batch<const N: usize>() {
    let module = compile_source(STATIC_FIELD_SOURCE, 140 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 2_048,
                quantum: 2_048,
            },
        )
        .expect("static field fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes traced field plans, hidden closures, and Proxy definitions with one dispatch batch.
fn assert_instance_field_batch<const N: usize>() {
    let module = compile_source(INSTANCE_FIELD_SOURCE, 160 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("instance field fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes ordered static fields/blocks and captured block locals with one dispatch batch.
fn assert_static_block_batch<const N: usize>() {
    let module = compile_source(STATIC_BLOCK_SOURCE, 170 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("static block fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_class_promise_fixture(source_id: u32) -> CompiledModule {
    compile_source(CLASS_PROMISE_SOURCE, source_id)
}

fn compile_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("class-promise-batch"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("class fixture compiles")
}
