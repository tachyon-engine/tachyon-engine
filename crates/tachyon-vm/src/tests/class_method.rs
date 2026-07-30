use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{ForcedCollectionMode, SPAN_SIZE_BYTES};

use super::super::*;

const CLASS_METHOD_KIND_SOURCE: &str = r#"
var classMethodResult = true;
var classMethodSettled = 0;
var classMethodCheckIndex = 0;
var classMethodFailures = "";
function check(value) {
  if (!value) classMethodFailures += classMethodCheckIndex + "|";
  classMethodCheckIndex = classMethodCheckIndex + 1;
  classMethodResult = classMethodResult && value;
}
function settles(expected) {
  return function(value) {
    check(value === expected);
    classMethodSettled = classMethodSettled + 1;
  };
}
function rejects() { classMethodResult = false; }
function hasOwnPrototype(value) {
  return Object.prototype.hasOwnProperty.call(value, "prototype");
}
function cannotConstruct(value) {
  try { new value(); } catch (error) { return error instanceof TypeError; }
  return false;
}

class Base {
  ordinary() { return this.bias + 1; }
  *generator() { yield this.bias + 2; }
  async asyncMethod() { return this.bias + 3; }
  async *asyncGenerator() { yield this.bias + 4; }

  static ordinary() { return this.staticBias + 10; }
  static *generator() { yield this.staticBias + 20; }
  static async asyncMethod() { return this.staticBias + 30; }
  static async *asyncGenerator() { yield this.staticBias + 40; }
}

class Derived extends Base {
  ordinary() { return super.ordinary() + 100; }
  *generator() { yield super.generator().next().value + 100; }
  async asyncMethod() { return super.ordinary() + 102; }
  async *asyncGenerator() {
    yield super.generator().next().value + 102;
  }

  static ordinary() { return super.ordinary() + 100; }
  static *generator() { yield super.generator().next().value + 100; }
  static async asyncMethod() { return super.ordinary() + 120; }
  static async *asyncGenerator() {
    yield super.generator().next().value + 120;
  }

  #ordinary() { return super.ordinary() + 200; }
  *#generator() { yield super.generator().next().value + 200; }
  async #asyncMethod() { return super.ordinary() + 202; }
  async *#asyncGenerator() {
    yield super.generator().next().value + 202;
  }

  static #staticOrdinary() { return super.ordinary() + 200; }
  static *#staticGenerator() { yield super.generator().next().value + 200; }
  static async #staticAsyncMethod() { return super.ordinary() + 220; }
  static async *#staticAsyncGenerator() {
    yield super.generator().next().value + 220;
  }

  privateOrdinary() { return this.#ordinary(); }
  privateGenerator() { return this.#generator(); }
  privateAsync() { return this.#asyncMethod(); }
  privateAsyncGenerator() { return this.#asyncGenerator(); }
  privateFunctions() {
    return [this.#ordinary, this.#generator, this.#asyncMethod, this.#asyncGenerator];
  }

  static privateOrdinary() { return this.#staticOrdinary(); }
  static privateGenerator() { return this.#staticGenerator(); }
  static privateAsync() { return this.#staticAsyncMethod(); }
  static privateAsyncGenerator() { return this.#staticAsyncGenerator(); }
  static privateFunctions() {
    return [
      this.#staticOrdinary,
      this.#staticGenerator,
      this.#staticAsyncMethod,
      this.#staticAsyncGenerator
    ];
  }
}

Derived.staticBias = 5;
var instance = new Derived();
instance.bias = 7;

var publicFunctions = [
  Derived.prototype.ordinary,
  Derived.prototype.generator,
  Derived.prototype.asyncMethod,
  Derived.prototype.asyncGenerator,
  Derived.ordinary,
  Derived.generator,
  Derived.asyncMethod,
  Derived.asyncGenerator
];
var privateFunctions = instance.privateFunctions();
var privateStaticFunctions = Derived.privateFunctions();
var allFunctions = publicFunctions;
for (var privateIndex = 0; privateIndex < privateFunctions.length; privateIndex = privateIndex + 1) {
  allFunctions.push(privateFunctions[privateIndex]);
  allFunctions.push(privateStaticFunctions[privateIndex]);
}
for (var index = 0; index < allFunctions.length; index = index + 1) {
  check(cannotConstruct(allFunctions[index]));
  var privateKindIndex = (index - 8) % 8;
  var hasGeneratorPrototype = index < 8
    ? index % 4 === 1 || index % 4 === 3
    : privateKindIndex === 2 || privateKindIndex === 3 ||
      privateKindIndex === 6 || privateKindIndex === 7;
  check(hasOwnPrototype(allFunctions[index]) === hasGeneratorPrototype);
}

check(instance.ordinary() === 108);
check(instance.generator().next().value === 109);
check(Derived.ordinary() === 115);
check(Derived.generator().next().value === 125);
check(instance.privateOrdinary() === 208);
check(instance.privateGenerator().next().value === 209);
check(Derived.privateOrdinary() === 215);
check(Derived.privateGenerator().next().value === 225);

instance.asyncMethod().then(settles(110), rejects);
instance.asyncGenerator().next().then(function(step) { settles(111)(step.value); }, rejects);
Derived.asyncMethod().then(settles(135), rejects);
Derived.asyncGenerator().next().then(function(step) { settles(145)(step.value); }, rejects);
instance.privateAsync().then(settles(210), rejects);
instance.privateAsyncGenerator().next().then(function(step) { settles(211)(step.value); }, rejects);
Derived.privateAsync().then(settles(235), rejects);
Derived.privateAsyncGenerator().next().then(function(step) { settles(245)(step.value); }, rejects);
"scheduled";
"#;

const CLASS_METHOD_KIND_ASSERTION: &str = "classMethodFailures + ':' + classMethodCheckIndex + ':' + classMethodSettled + ':' + classMethodResult;";

#[test]
fn class_method_roles_execute_for_every_dispatch_batch_and_forced_major() {
    assert_class_method_kinds::<1>(false);
    assert_class_method_kinds::<2>(false);
    assert_class_method_kinds::<4>(false);
    assert_class_method_kinds::<8>(false);
    assert_class_method_kinds::<16>(false);
    assert_class_method_kinds::<8>(true);
}

#[test]
fn class_async_method_minimal_probes() {
    for (source, expected, source_id) in [
        (
            "var probe = 0; class C { async value() { return 1; } } new C().value().then(function(value) { probe = value; }); true;",
            "probe === 1;",
            8_800,
        ),
        (
            "var probe = 0; class B { value() { return 1; } } class C extends B { async value() { return super.value() + 1; } } new C().value().then(function(value) { probe = value; }); true;",
            "probe === 2;",
            8_810,
        ),
        (
            "var probe = 0; class C { async *value() { yield 1; } } var method = C.prototype.value; var descriptor = Object.getOwnPropertyDescriptor(method, 'prototype'); var generator = new C().value(); var shapeOk = descriptor.writable && !descriptor.enumerable && !descriptor.configurable && Object.getPrototypeOf(generator) === method.prototype && typeof generator.next === 'function'; generator.next().then(function(step) { probe = step.value; }); shapeOk;",
            "probe === 1;",
            8_820,
        ),
        (
            "var probe = 0; class B { *value() { yield 1; } } class C extends B { async *value() { yield super.value().next().value + 1; } } new C().value().next().then(function(step) { probe = step.value; }); true;",
            "probe === 2;",
            8_830,
        ),
    ] {
        assert_class_method_probe(source, expected, source_id, false);
        assert_class_method_probe(source, expected, source_id + 100, true);
    }
}

#[test]
fn class_generator_method_prototype_contracts() {
    for (prefix, source_id) in [("*", 8_840), ("async *", 8_850)] {
        let source = format!(
            r#"
class C {{ {prefix}value() {{ yield 1; }} }}
var method = C.prototype.value;
var descriptor = Object.getOwnPropertyDescriptor(method, "prototype");
var keys = Reflect.ownKeys(method);
var prototypeKeyCount = 0;
for (var index = 0; index < keys.length; index = index + 1) {{
  if (keys[index] === "prototype") prototypeKeyCount = prototypeKeyCount + 1;
}}
var originalPrototype = method.prototype;
var fallbackPrototype = Object.getPrototypeOf(originalPrototype);
var replacement = {{ replacement: true }};
method.prototype = replacement;
var replacementApplied = method.prototype === replacement;
var replacementInstance = new C().value();
var replacementUsed = Object.getPrototypeOf(replacementInstance) === replacement;
var deletionRejected = delete method.prototype === false && method.prototype === replacement;
method.prototype = 1;
var fallbackInstance = new C().value();
descriptor.writable && !descriptor.enumerable && !descriptor.configurable &&
  prototypeKeyCount === 1 && replacementApplied && replacementUsed && deletionRejected &&
  Object.getPrototypeOf(fallbackInstance) === fallbackPrototype;
"#,
        );
        assert_class_method_probe(&source, "true;", source_id, false);
        let forced_source = format!(
            r#"
class C {{ {prefix}value() {{ yield 1; }} }}
var method = C.prototype.value;
var fallbackPrototype = Object.getPrototypeOf(method.prototype);
method.prototype = 1;
Object.getPrototypeOf(new C().value()) === fallbackPrototype;
"#,
        );
        assert_class_method_probe(&forced_source, "true;", source_id + 100, true);
    }
}

/// Executes one minimal async class-method case and checks its post-checkpoint global state.
fn assert_class_method_probe(source: &str, expected: &str, source_id: u32, forced_major: bool) {
    let compiler = Compiler;
    let setup = compile_class_method_source(&compiler, source, source_id, "class-method-probe");
    let assertion = compile_class_method_source(
        &compiler,
        expected,
        source_id + 1,
        "class-method-probe-assertion",
    );
    let mut isolate = class_method_isolate(forced_major);
    let setup = execute_class_method_source::<8>(&mut isolate, &setup, "probe setup");
    let RunOutcome::Completed(_) = setup else {
        panic!("class method probe {source_id} setup returned {setup:?}");
    };
    let outcome = execute_class_method_source::<8>(&mut isolate, &assertion, "probe assertion");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "class method probe {source_id} assertion returned {outcome:?}"
    );
}

/// Runs all class-method execution kinds through a complete Promise-job checkpoint.
fn assert_class_method_kinds<const N: usize>(forced_major: bool) {
    let compiler = Compiler;
    let setup = compile_class_method_source(
        &compiler,
        CLASS_METHOD_KIND_SOURCE,
        8_600 + N as u32 + u32::from(forced_major) * 100,
        "class-method-kinds",
    );
    let assertion = compile_class_method_source(
        &compiler,
        CLASS_METHOD_KIND_ASSERTION,
        8_700 + N as u32 + u32::from(forced_major) * 100,
        "class-method-kinds-assertion",
    );
    let mut isolate = class_method_isolate(forced_major);
    let setup_outcome = execute_class_method_source::<N>(&mut isolate, &setup, "setup");
    assert!(
        matches!(setup_outcome, RunOutcome::Completed(_)),
        "class method setup batch {N}, forced_major={forced_major} returned {setup_outcome:?}"
    );
    let outcome = execute_class_method_source::<N>(&mut isolate, &assertion, "assertion");
    let RunOutcome::Completed(value) = outcome else {
        panic!("class method batch {N}, forced_major={forced_major} returned {outcome:?}");
    };
    let value = isolate
        .string_value_to_utf16(value)
        .expect("class method assertion is a diagnostic string");
    assert_eq!(
        String::from_utf16(&value).expect("class method diagnostic is valid UTF-16"),
        ":48:8:true",
        "class method batch {N}, forced_major={forced_major}"
    );
}

/// Compiles one immutable half of the class-method setup/assertion pair.
fn compile_class_method_source(
    compiler: &Compiler,
    source: &str,
    source_id: u32,
    name: &'static str,
) -> CompiledModule {
    compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new(name),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("class method fixture compiles")
}

/// Creates the bounded fixture isolate and enables allocation-by-allocation major collection.
fn class_method_isolate(forced_major: bool) -> Isolate {
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(4_096, 4 * 1024 * 1024, AtomHashSeed::new(67, 68)),
        HeapLimit::new(192 * SPAN_SIZE_BYTES),
        StackLimits::new(192, 16_384),
        RealmLimits::new(128, 4_096),
    ))
    .expect("class method isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    isolate
}

/// Executes one module with enough fuel to drain every nested async-generator Promise job.
fn execute_class_method_source<const N: usize>(
    isolate: &mut Isolate,
    module: &CompiledModule,
    label: &str,
) -> RunOutcome {
    isolate
        .execute_with_batch::<N>(
            module,
            ExecutionBudget {
                fuel: 131_072,
                quantum: 131_072,
            },
        )
        .unwrap_or_else(|error| panic!("class method {label} executes: {error:?}"))
}
