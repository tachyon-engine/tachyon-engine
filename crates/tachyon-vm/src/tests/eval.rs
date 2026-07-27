use std::sync::Arc;

use tachyon_compiler::{
    CompileError, CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText,
};

use super::{fixtures::test_isolate, *};

/// Compiles and executes a host-provided eval script in the selected Realm.
fn eval_script_callback(
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

fn dynamic_function_callback(
    _isolate: &mut Isolate,
    _realm: RealmId,
) -> Result<Value, ExecutionError> {
    Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
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
        .expect("direct eval fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "batch {N} direct eval returned {outcome:?}"
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
