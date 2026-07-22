use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

/// Compiles and executes a host-provided eval script in the selected Realm.
fn eval_script_callback(
    isolate: &mut Isolate,
    realm: RealmId,
    source: Value,
) -> Result<Value, ExecutionError> {
    let units = isolate.string_value_to_utf16(source)?;
    let source = String::from_utf16_lossy(&units);
    let module = compile_source(&source, u32::MAX - 10);
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

fn dynamic_function_callback(
    _isolate: &mut Isolate,
    _realm: RealmId,
) -> Result<Value, ExecutionError> {
    Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
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
