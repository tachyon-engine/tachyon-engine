use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const SOURCE_TEXT_CASES: &str = r#"
function /* keep */ ordinary(a) { return a; }
var arrow = (x /* unicode \u0061 */) => x + 1;
var object = {
  method /* gap */ (x) { return x; },
  get value /* getter */ () { return 1; }
};
class Example extends Object { constructor() { super(); } method() {} }
Function.prototype.toString.call(ordinary) === "function /* keep */ ordinary(a) { return a; }" &&
Function.prototype.toString.call(arrow) === "(x /* unicode \\u0061 */) => x + 1" &&
Function.prototype.toString.call(object.method) === "method /* gap */ (x) { return x; }" &&
Function.prototype.toString.call(Object.getOwnPropertyDescriptor(object, "value").get) === "get value /* getter */ () { return 1; }" &&
Function.prototype.toString.call(Example) === "class Example extends Object { constructor() { super(); } method() {} }";
"#;

const NATIVE_CASES: &str = r#"
var builtin = Array.prototype.push;
Object.defineProperty(builtin, "name", { value: "changed" });
var symbolBuiltin = RegExp.prototype[Symbol.match];
var speciesGetter = Object.getOwnPropertyDescriptor(RegExp, Symbol.species).get;
var bound = (function target() {}).bind(null);
var proxy = new Proxy(function visible() {}, {});
var rejected = false;
try { Function.prototype.toString.call(new Proxy({}, {})); }
catch (error) { rejected = error instanceof TypeError; }
Function.prototype.toString.call(builtin) === "function push() { [native code] }" &&
Function.prototype.toString.call(symbolBuiltin) === "function [Symbol.match]() { [native code] }" &&
Function.prototype.toString.call(speciesGetter) === "function get [Symbol.species]() { [native code] }" &&
Function.prototype.toString.call(bound) === "function () { [native code] }" &&
Function.prototype.toString.call(proxy) === "function () { [native code] }" &&
"" + proxy === "function () { [native code] }" && rejected;
"#;

#[test]
fn returns_exact_source_for_functions_methods_and_classes() {
    assert_function_to_string_source::<1>(SOURCE_TEXT_CASES, 9_100, false);
    assert_function_to_string_source::<4>(SOURCE_TEXT_CASES, 9_104, false);
    assert_function_to_string_source::<8>(SOURCE_TEXT_CASES, 9_108, true);
    assert_function_to_string_source::<16>(SOURCE_TEXT_CASES, 9_116, true);
}

#[test]
fn uses_internal_names_and_hides_wrapped_sources() {
    assert_function_to_string_source::<2>(NATIVE_CASES, 9_202, false);
    assert_function_to_string_source::<8>(NATIVE_CASES, 9_208, true);
}

/// Executes one source-retention fixture under a selected dispatch and GC policy.
fn assert_function_to_string_source<const N: usize>(
    source: &str,
    source_id: u32,
    forced_major: bool,
) {
    let module = compile_source(source, source_id);
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
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("Function.prototype.toString fixture executes");
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
                SourceName::new("function-to-string"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Function.prototype.toString fixture compiles")
}
