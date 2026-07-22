use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

/// Compiles and executes a host-provided eval script in the selected Realm.
fn eval_script_callback(
    isolate: &mut Isolate,
    realm: RealmId,
    kind: EvalKind,
    source: Value,
) -> Result<Value, ExecutionError> {
    let units = isolate.string_value_to_utf16(source)?;
    let source = String::from_utf16_lossy(&units);
    let module = compile_source(&source, u32::MAX - 10);
    let budget = ExecutionBudget {
        fuel: 8_192,
        quantum: 8_192,
    };
    let outcome = match kind {
        EvalKind::Direct => isolate.execute_direct_eval_in_realm(realm, &module, budget),
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
