use std::sync::Arc;

use tachyon_compiler::{
    CompileError, CompileOptions, Compiler, DynamicFunctionKind as CompilerDynamicFunctionKind,
    MediaType, SourceId, SourceName, SourceText,
};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

/// Compiles and executes a host-provided eval script in the selected Realm.
pub(super) fn eval_script_callback(
    isolate: &mut Isolate,
    realm: RealmId,
    kind: EvalKind,
    source: Value,
) -> Result<Value, ExecutionError> {
    let units = isolate.string_value_to_utf16(source)?;
    let mut source = String::from_utf16_lossy(&units);
    if kind.inherits_strict() {
        const STRICT_PROLOGUE: &str = "\"use strict\";\nvoid 0;\n";
        source
            .try_reserve_exact(STRICT_PROLOGUE.len())
            .map_err(|_| ExecutionError::UnsupportedDynamicFunctionConstructor)?;
        source.insert_str(0, STRICT_PROLOGUE);
    }
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(u32::MAX - 10),
                SourceName::new("direct-eval"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions {
                direct_eval: matches!(kind, EvalKind::Direct { .. }),
                ..CompileOptions::default()
            },
        )
        .map_err(|error| match error {
            CompileError::Diagnostics(_) => ExecutionError::InvalidEvalSource,
            _ => ExecutionError::UnsupportedDynamicFunctionConstructor,
        })?;
    let budget = ExecutionBudget {
        fuel: 8_192,
        quantum: 8_192,
    };
    let outcome = match kind {
        EvalKind::Direct { .. } => {
            isolate.execute_direct_eval_in_realm(realm, &module, budget, kind.inherits_strict())
        }
        EvalKind::Indirect => isolate.execute_in_realm(realm, &module, budget),
    }?;
    match outcome {
        RunOutcome::Completed(value) => Ok(value),
        RunOutcome::Thrown(value) => Err(ExecutionError::HostThrown(value)),
        RunOutcome::BudgetExhausted => Err(ExecutionError::UnsupportedDynamicFunctionConstructor),
    }
}

pub(super) fn dynamic_function_callback(
    isolate: &mut Isolate,
    realm: RealmId,
    kind: crate::DynamicFunctionKind,
    source: crate::DynamicFunctionSource,
) -> Result<Value, ExecutionError> {
    let kind = match kind {
        crate::DynamicFunctionKind::Ordinary => CompilerDynamicFunctionKind::Ordinary,
        crate::DynamicFunctionKind::Generator => CompilerDynamicFunctionKind::Generator,
        crate::DynamicFunctionKind::Async => CompilerDynamicFunctionKind::Async,
        crate::DynamicFunctionKind::AsyncGenerator => CompilerDynamicFunctionKind::AsyncGenerator,
    };
    let module = Compiler
        .compile_dynamic_function(
            SourceId::new(u32::MAX - 11),
            SourceName::new("dynamic-function"),
            kind,
            &source.parameters,
            &source.body,
        )
        .map_err(|error| match error {
            CompileError::Diagnostics(_) => ExecutionError::InvalidEvalSource,
            _ => ExecutionError::UnsupportedDynamicFunctionConstructor,
        })?;
    match isolate.execute_in_realm(
        realm,
        &module,
        ExecutionBudget {
            fuel: 8_192,
            quantum: 8_192,
        },
    )? {
        RunOutcome::Completed(value) => Ok(value),
        RunOutcome::Thrown(value) => Err(ExecutionError::HostThrown(value)),
        RunOutcome::BudgetExhausted => Err(ExecutionError::UnsupportedDynamicFunctionConstructor),
    }
}

/// Exercises observable argument conversion across every dispatch batch and an exact major GC.
fn assert_dynamic_function_batch<const N: usize>(forced_major: bool) {
    let module = compile_source(
        "var trace = ''; var p = { toString() { trace += 'p'; return 'value'; } }; var b = { toString() { trace += 'b'; return 'return value + 1;'; } }; var f = Function(p, b); trace === 'pb' && f.name === 'anonymous' && f.length === 1 && f(2) === 3;",
        1_169,
    );
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("dynamic-function hooks install");
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
        .expect("dynamic Function executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

#[test]
fn dynamic_function_argument_conversion_is_resumable_for_every_batch() {
    assert_dynamic_function_batch::<1>(false);
    assert_dynamic_function_batch::<2>(false);
    assert_dynamic_function_batch::<4>(false);
    assert_dynamic_function_batch::<8>(true);
    assert_dynamic_function_batch::<16>(true);
}

#[test]
fn dynamic_function_reads_and_writes_its_constructor_realm_global_object() {
    let module = compile_source(
        "var other = $262.createRealm().global; other.calls = 0; var fn = other.Function('calls += 1;'); fn(); other.calls;",
        1_171,
    );
    // The dynamic function keeps two complete Realm graphs live during forced major GC.
    let mut isolate = test_isolate_with_heap_spans(10);
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("dynamic-function hooks install");
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("cross-Realm dynamic Function executes");
    assert_eq!(outcome, RunOutcome::Completed(Value::from_i32(1)));
}

#[test]
fn cross_realm_sloppy_this_uses_callee_wrappers_for_every_dispatch_batch() {
    assert_cross_realm_this_binding::<1>(false);
    assert_cross_realm_this_binding::<2>(false);
    assert_cross_realm_this_binding::<4>(false);
    assert_cross_realm_this_binding::<8>(false);
    assert_cross_realm_this_binding::<16>(false);
    assert_cross_realm_this_binding::<1>(true);
    assert_cross_realm_this_binding::<2>(true);
    assert_cross_realm_this_binding::<4>(true);
    assert_cross_realm_this_binding::<8>(true);
    assert_cross_realm_this_binding::<16>(true);
}

#[test]
fn dynamic_function_to_string_returns_the_canonical_generated_source() {
    let module = compile_source(
        r#"var f = Function("a", " /* a */ b, c /* b */ //", "/* c */ ; /* d */ //");
Function.prototype.toString.call(f) === "function anonymous(a, /* a */ b, c /* b */ //\n) {\n/* c */ ; /* d */ //\n}";"#,
        1_173,
    );
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("dynamic-function hooks install");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("dynamic Function source executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dynamic Function source returned {outcome:?}"
    );
}

/// Proves native and bound callables retain `[[Realm]]` after all prototype clues are removed.
#[test]
fn dynamic_function_new_target_uses_explicit_callable_realm() {
    let module = compile_source(
        r#"
var other = $262.createRealm().global;
var childFunction = other.Function;
var bound = childFunction.bind(null);
Object.setPrototypeOf(childFunction, null);
Object.setPrototypeOf(bound, null);
var throughBound = Reflect.construct(Function, ["return 2;"], bound);
Object.getPrototypeOf(throughBound) === other.Function.prototype;
"#,
        1_175,
    );
    let mut isolate = test_isolate_with_heap_spans(18);
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("dynamic-function hooks install");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("explicit callable Realm fixture executes");
    if let RunOutcome::Thrown(value) = outcome {
        panic!("thrown {value:?}: {:?}", isolate.native_error_kind(value));
    }
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True))
    );
}

/// Separates newTarget-Realm function inheritance from constructor-Realm generator inheritance.
fn assert_cross_realm_generator_prototype_graph(source_id: u32, literal: &str) {
    let source = format!(
        r#"
var a = $262.createRealm().global;
var af = a.eval("({literal})");
var agf = af.constructor;
var agp = Object.getPrototypeOf(af.prototype);
var b = $262.createRealm().global;
var bf = b.eval("({literal})");
var bgf = bf.constructor;
var nt = new b.Function();
nt.prototype = null;
Object.setPrototypeOf(agf, null);
var fn = Reflect.construct(agf, ["yield 1;"], nt);
(Object.getPrototypeOf(fn) === bgf.prototype ? 1 : 0) +
    (Object.getPrototypeOf(fn.prototype) === agp ? 2 : 0);
"#
    );
    let module = compile_source(&source, source_id);
    let mut isolate = test_isolate_with_heap_spans(18);
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("dynamic-function hooks install");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("cross-Realm generator prototype fixture executes");
    assert_eq!(outcome, RunOutcome::Completed(Value::from_i32(3)));
}

#[test]
fn cross_realm_dynamic_generator_prototype_graph_is_split_by_spec_role() {
    assert_cross_realm_generator_prototype_graph(1_172, "function*(){}");
}

#[test]
fn cross_realm_dynamic_async_generator_prototype_graph_is_split_by_spec_role() {
    assert_cross_realm_generator_prototype_graph(1_174, "async function*(){}");
}

/// Keeps every freshly installed child-Realm host hook live across exact major safepoints.
#[test]
fn cross_realm_eval_generator_literal_survives_forced_major_gc() {
    let module = compile_source(
        "var a = $262.createRealm().global; typeof a.eval('(function*(){})') === 'function';",
        1_173,
    );
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("eval hooks install");
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("cross-Realm eval generator literal executes");
    if let RunOutcome::Thrown(value) = outcome {
        panic!("thrown {value:?}: {:?}", isolate.native_error_kind(value));
    }
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "cross-Realm eval returned {outcome:?}"
    );
}

/// Runs one direct-eval fixture through a selected dispatch monomorphization.
fn assert_direct_eval_batch<const N: usize>(module: &CompiledModule, forced_major: bool) {
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("eval hooks install");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            module,
            ExecutionBudget {
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .unwrap_or_else(|error| {
            panic!("batch {N} direct eval forced_major={forced_major} failed: {error:?}")
        });
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "batch {N} direct eval returned {outcome:?}"
    );
}

/// Executes callee-Realm global substitution and primitive boxing under one dispatch policy.
fn assert_cross_realm_this_binding<const N: usize>(forced_major: bool) {
    let module = compile_source(
        r#"
var other = $262.createRealm().global;
var sloppy = other.Function("return this;");
var strict = other.Function("'use strict'; return this;");
var number = sloppy.call(1);
var boolean = sloppy.apply(false);
var string = sloppy.call("");
var generator = other.eval("(function*(){ yield this; })");
var yielded = generator.call(2).next().value;
var directObject = new other.Object();
var newTarget = other.Function();
newTarget.prototype = 1;
var nestedBoundNewTarget = newTarget.bind().bind();
var reflectedObject = Reflect.construct(other.Object, [], nestedBoundNewTarget);
sloppy.call(null) === other &&
Object.getPrototypeOf(number) === other.Number.prototype &&
Object.getPrototypeOf(boolean) === other.Boolean.prototype &&
Object.getPrototypeOf(string) === other.String.prototype &&
Object.getPrototypeOf(yielded) === other.Number.prototype && yielded.valueOf() === 2 &&
Object.getPrototypeOf(directObject) === other.Object.prototype &&
Object.getPrototypeOf(reflectedObject) === other.Object.prototype &&
strict.call(1) === 1;
"#,
        1_180 + N as u32 + u32::from(forced_major) * 100,
    );
    let mut isolate = test_isolate_with_heap_spans(18);
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("cross-Realm hooks install");
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
        .unwrap_or_else(|error| {
            panic!("dispatch batch {N}, forced_major={forced_major} failed to execute: {error:?}")
        });
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

#[test]
fn standalone_string_expression_returns_its_completion_value() {
    let module = compile_source("\"bj\";", 1_159);
    let outcome = test_isolate()
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 128,
                quantum: 128,
            },
        )
        .expect("standalone expression executes");
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_heap_ref().is_some()));
}

#[test]
fn host_eval_script_returns_nested_completion_value() {
    let module = compile_source("eval('\\\"bj\\\"') === 'bj';", 1_160);
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("eval hooks install");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("eval fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "eval fixture returned {outcome:?}"
    );
}

#[test]
fn host_eval_script_propagates_nested_throw_to_outer_handler() {
    let module = compile_source(
        "var caught = false; try { eval('throw 3;'); } catch (error) { caught = error === 3; } caught;",
        1_161,
    );
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("eval hooks install");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("eval throw fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "eval throw fixture returned {outcome:?}"
    );
}

#[test]
fn direct_eval_reads_and_updates_parameter_var_and_lexical_bindings() {
    let module = compile_source(
        "function run(param) { var local = 1; let lexical = 2; eval('param = param + local + lexical; local = 4; lexical = 5;'); return param === 6 && local === 4 && lexical === 5; } run(3);",
        1_162,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn eval_alias_remains_indirect_and_cannot_observe_caller_lexicals() {
    let module = compile_source(
        "function run() { let hidden = 1; let alias = eval; return alias('typeof hidden') === 'undefined'; } run();",
        1_163,
    );
    assert_direct_eval_batch::<8>(&module, false);
}

#[test]
fn direct_and_indirect_eval_return_non_string_inputs_without_coercion() {
    let module = compile_source(
        "var object = {}; eval(7) === 7 && eval(object) === object && (0, eval)(null) === null;",
        1_164,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn direct_eval_inherits_caller_strictness_for_parse_and_assignment_rules() {
    let module = compile_source(
        "function run() { 'use strict'; var syntax = false; var reference = false; try { eval('var public = 1;'); } catch (error) { syntax = error instanceof SyntaxError; } try { eval('missing = 1;'); } catch (error) { reference = error instanceof ReferenceError; } return syntax && reference; } run();",
        1_165,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn sloppy_eval_persists_new_var_and_function_bindings_on_the_caller_activation() {
    let module = compile_source(
        "function run() { var existing = 1; eval('var existing = 2; var added = 3; function created() { return added; }'); var first = existing === 2 && added === 3 && created() === 3; eval('added = 5;'); return first && added === 5 && created() === 5; } run();",
        1_166,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn strict_eval_var_and_function_bindings_do_not_escape_the_eval_fiber() {
    let module = compile_source(
        "function run() { 'use strict'; eval('var hidden = 1; function created() {}'); return typeof hidden === 'undefined' && typeof created === 'undefined'; } run();",
        1_167,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn global_direct_eval_uses_global_var_environment_only_when_sloppy() {
    let sloppy = compile_source(
        "eval('var globalEval = 3; function globalCreated() { return 4; }'); globalEval === 3 && globalCreated() === 4;",
        1_168,
    );
    assert_direct_eval_batch::<1>(&sloppy, false);
    assert_direct_eval_batch::<8>(&sloppy, true);
    let strict = compile_source(
        "'use strict'; eval('var isolated = 1; function hidden() {}'); typeof isolated === 'undefined' && typeof hidden === 'undefined';",
        1_169,
    );
    assert_direct_eval_batch::<2>(&strict, false);
    assert_direct_eval_batch::<4>(&strict, false);
    assert_direct_eval_batch::<16>(&strict, true);
}

#[test]
fn nested_sloppy_eval_var_shadows_an_ancestor_eval_overlay() {
    let module = compile_source(
        "function outer() { eval('var value = 1;'); function inner() { eval('var value = 2;'); return value; } return inner() === 2 && value === 1; } outer();",
        1_170,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn sloppy_eval_cannot_cross_a_non_simple_parameter_environment() {
    let module = compile_source(
        "var body = false; function rejected(a = eval('var a = 42')) { body = true; } var syntax = false; try { rejected(); } catch (error) { syntax = error instanceof SyntaxError; } function simple(a) { eval('var a = 42'); return a; } syntax && !body && simple(1) === 42;",
        1_175,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn direct_eval_lexical_record_enforces_tdz_const_and_escaping_closure_capture() {
    let module = compile_source(
        "var escaped; var assignment = false; eval('let hidden; const fixed = 4; escaped = function() { return fixed; }; try { fixed = 5; } catch (error) { assignment = error instanceof TypeError; }'); typeof hidden === 'undefined' && assignment && escaped() === 4;",
        1_171,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
/// Keeps named-function self bindings non-strict without weakening strict immutable bindings.
fn direct_eval_reassignment_obeys_function_name_binding_strictness() {
    let module = compile_source(
        r#"
var ordinary;
ordinary = function OrdinaryName() {
    eval("OrdinaryName = 1");
    return OrdinaryName === ordinary;
};
var strictOrdinary = function StrictOrdinaryName() {
    "use strict";
    try { eval("StrictOrdinaryName = 1"); }
    catch (error) { return error instanceof TypeError; }
    return false;
};
function constBindingRemainsStrict() {
    const fixed = 1;
    try { eval("fixed = 2"); }
    catch (error) { return error instanceof TypeError; }
    return false;
}
ordinary() && strictOrdinary() && constBindingRemainsStrict();
"#,
        1_176,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);

    let generator = compile_source(
        r#"
var generator;
generator = function* GeneratorName() {
    eval("GeneratorName = 1");
    return GeneratorName === generator;
};
var strictGenerator = function* StrictGeneratorName() {
    "use strict";
    try { eval("StrictGeneratorName = 1"); }
    catch (error) { return error instanceof TypeError; }
    return false;
};
generator().next().value && strictGenerator().next().value;
"#,
        1_177,
    );
    assert_direct_eval_batch::<1>(&generator, false);
    assert_direct_eval_batch::<2>(&generator, false);
    assert_direct_eval_batch::<4>(&generator, false);
    assert_direct_eval_batch::<8>(&generator, false);
    assert_direct_eval_batch::<16>(&generator, false);
}

#[test]
fn direct_eval_spread_retains_caller_scope_for_every_dispatch_batch() {
    let module = compile_source(
        "var value = 1; var trace = ''; function extra() { trace += 'e'; return 0; } eval(...['value = 7'], extra()); value === 7 && trace === 'e';",
        1_172,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<2>(&module, false);
    assert_direct_eval_batch::<4>(&module, false);
    assert_direct_eval_batch::<8>(&module, true);
    assert_direct_eval_batch::<16>(&module, true);
}

#[test]
fn sloppy_arguments_indices_alias_simple_parameters() {
    let module = compile_source(
        "function foo(a, b, c) { a = 1; b = 'str'; c = 2.1; return arguments[0] === 1 && arguments[1] === 'str' && arguments[2] === 2.1; } function bar(a) { arguments[0] = 7; return a === 7; } foo(10, 'sss', 1) && bar(3);",
        1_173,
    );
    assert_direct_eval_batch::<1>(&module, false);
    assert_direct_eval_batch::<1>(&module, true);
}

fn compile_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("eval-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("eval fixture compiles")
}
