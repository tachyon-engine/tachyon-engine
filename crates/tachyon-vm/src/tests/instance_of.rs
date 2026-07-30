use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const INSTANCE_OF_SOURCE: &str = r#"
"use strict";
var marker = {};
var trace = "";
var custom = {};
Object.defineProperty(custom, Symbol.hasInstance, {
  configurable: true,
  get() {
    trace += "g";
    return function(value) {
      trace += this === custom ? "c" : "x";
      return value === marker;
    };
  }
});
var customResult = marker instanceof custom;

function Target() {}
var bound = Target.bind(null);
Object.defineProperty(Target, Symbol.hasInstance, {
  configurable: true,
  value(value) {
    trace += "b";
    return this === Target && value === marker;
  }
});
var boundResult = Function.prototype[Symbol.hasInstance].call(bound, marker);

var prototype = {};
var accessor = Object.getOwnPropertyDescriptor({ get value() {} }, "value").get;
Object.defineProperty(accessor, "prototype", {
  configurable: true,
  get() { trace += "a"; return prototype; }
});
var prototypeResult = Function.prototype[Symbol.hasInstance].call(
  accessor,
  Object.create(prototype)
);

function Constructor() {}
var proxy = new Proxy(Object.create(Constructor.prototype), {
  getPrototypeOf(target) {
    trace += "p";
    return Reflect.getPrototypeOf(target);
  }
});
var proxyResult = Constructor[Symbol.hasInstance](proxy);

var token = {};
var abrupt = false;
var throwing = {};
Object.defineProperty(throwing, Symbol.hasInstance, {
  get() { throw token; }
});
try { marker instanceof throwing; } catch (error) { abrupt = error === token; }

customResult && boundResult && prototypeResult && proxyResult && abrupt && trace === "gcbap";
"#;

#[test]
fn instance_of_operator_and_builtin_resume_for_every_dispatch_batch() {
    assert_instance_of_source::<1>(10_101, false);
    assert_instance_of_source::<2>(10_102, false);
    assert_instance_of_source::<4>(10_104, false);
    assert_instance_of_source::<8>(10_108, true);
    assert_instance_of_source::<16>(10_116, true);
}

/// Executes the full method/prototype/Proxy continuation matrix under one VM policy.
fn assert_instance_of_source<const N: usize>(source_id: u32, forced_major: bool) {
    let module = compile_source(INSTANCE_OF_SOURCE, source_id);
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
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("instanceof continuation fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

fn compile_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("instanceof"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("instanceof fixture compiles")
}
