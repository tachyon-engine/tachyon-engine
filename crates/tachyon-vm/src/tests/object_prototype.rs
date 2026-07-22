use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const OBJECT_VALUE_OF_SOURCE: &str = r#"
var object = {};
var valueOf = Object.prototype.valueOf;
var descriptor = Object.getOwnPropertyDescriptor(Object.prototype, "valueOf");
var nullishThrows = false;
var constructThrows = false;
try { valueOf.call(null); } catch (error) { nullishThrows = error instanceof TypeError; }
try { new valueOf(); } catch (error) { constructThrows = error instanceof TypeError; }
valueOf.call(object) === object &&
valueOf.call(7).valueOf() === 7 &&
valueOf.call("wide").valueOf() === "wide" &&
valueOf.call(Symbol("s")).valueOf().description === "s" &&
typeof valueOf.call(true) === "object" &&
valueOf.call(true).valueOf() === true &&
new Boolean(false).valueOf() === false &&
new Boolean(true).toString() === "true" &&
Object(true).valueOf() === true &&
Object.prototype.toString.call(new Boolean(false)) === "[object Boolean]" &&
!Object.getOwnPropertyDescriptor(Boolean, "prototype").writable &&
valueOf.name === "valueOf" && valueOf.length === 0 &&
descriptor.value === valueOf && descriptor.writable &&
!descriptor.enumerable && descriptor.configurable &&
nullishThrows && constructThrows;
"#;

const OBJECT_TO_LOCALE_STRING_SOURCE: &str = r#"
"use strict";
var trace = "";
var locale = Object.prototype.toLocaleString;
var plain = { toString() { trace += "c"; return this === plain ? "plain" : "bad"; } };
var plainResult = locale.call(plain);
Object.defineProperty(Boolean.prototype, "toString", {
  configurable: true,
  get() {
    trace += "g";
    var kind = typeof this;
    return function() { trace += "b"; return kind + ":" + typeof this; };
  }
});
var primitiveResult = locale.call(true);
var proxy;
var target = { toString() { trace += "p"; return this === proxy ? "proxy" : "bad"; } };
proxy = new Proxy(target, {
  get(target, key, receiver) { trace += "x"; return Reflect.get(target, key, receiver); }
});
var proxyResult = locale.call(proxy);
var nonCallableThrows = false;
try { locale.call({ toString: 1 }); } catch (error) { nonCallableThrows = error instanceof TypeError; }
var marker = {};
var abrupt = false;
try { locale.call({ get toString() { throw marker; } }); } catch (error) { abrupt = error === marker; }
plainResult === "plain" && primitiveResult === "boolean:boolean" &&
proxyResult === "proxy" && trace === "cgbxp" &&
nonCallableThrows && abrupt && locale.name === "toLocaleString" && locale.length === 0;
"#;

#[test]
fn object_value_of_executes_for_every_dispatch_batch() {
    assert_object_value_of_batch::<1>();
    assert_object_value_of_batch::<2>();
    assert_object_value_of_batch::<4>();
    assert_object_value_of_batch::<8>();
    assert_object_value_of_batch::<16>();
}

#[test]
/// Forces collection through every primitive wrapper allocation in the Object ToObject path.
fn object_value_of_boxing_survives_forced_major_collections() {
    let module = compile_object_value_of_source(106);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("forced-major Object.prototype.valueOf fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn object_to_locale_string_resumes_for_every_dispatch_batch() {
    assert_to_locale_string_batch::<1>();
    assert_to_locale_string_batch::<2>();
    assert_to_locale_string_batch::<4>();
    assert_to_locale_string_batch::<8>();
    assert_to_locale_string_batch::<16>();
}

#[test]
/// Forces collection through nested getter, Proxy, and method-call continuations.
fn object_to_locale_string_survives_forced_major_collections() {
    let module = compile_object_source(OBJECT_TO_LOCALE_STRING_SOURCE, 116);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major Object.prototype.toLocaleString fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Executes the Object.prototype.valueOf contract with one selected dispatch monomorphization.
fn assert_object_value_of_batch<const N: usize>() {
    let module = compile_object_value_of_source(100 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("Object.prototype.valueOf fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_object_value_of_source(source_id: u32) -> CompiledModule {
    compile_object_source(OBJECT_VALUE_OF_SOURCE, source_id)
}

/// Executes the observable Get/Call sequence with one selected dispatch monomorphization.
fn assert_to_locale_string_batch<const N: usize>() {
    let module = compile_object_source(OBJECT_TO_LOCALE_STRING_SOURCE, 110 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("Object.prototype.toLocaleString fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_object_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("object-prototype"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Object.prototype fixture compiles")
}
