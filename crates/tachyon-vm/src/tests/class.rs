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
        "forced-major class fixture returned {outcome:?}"
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
