use std::sync::Arc;

use tachyon_compiler::{
    CompileError, CompileOptions, Compiler, MediaType, SourceId, SourceMode, SourceName, SourceText,
};
use tachyon_gc::HeapLimit;
use tachyon_vm::{
    AtomHashSeed, AtomTableConfig, ExecutionBudget, Isolate, IsolateConfig, RealmLimits,
    RunOutcome, StackLimits,
};

use crate::{EngineAdapter, EngineOutcome, EngineResponse, ExecutionRequest, Phase, SourceUnit};

const ATOM_MAX_ENTRIES: u32 = 1 << 18;
const ATOM_MAX_BYTES: usize = 32 * 1024 * 1024;
// Test262 variants execute in-process, so a bounded per-variant budget keeps one unsupported
// infinite loop from suppressing a complete report. Passing conformance tests stay well below it.
const EXECUTION_FUEL_LIMIT: u64 = 100_000;
const HEAP_LIMIT_BYTES: usize = 256 * 1024 * 1024;
const STACK_MAX_FRAMES: u32 = 4_096;
const STACK_MAX_REGISTERS: u32 = 2 * 1024 * 1024;
const MAX_LOADED_MODULES: u32 = 64;
const MAX_GLOBAL_BINDINGS: u32 = 1 << 18;

/// Stateless in-process Test262 adapter; each request owns an independent Tachyon isolate.
#[derive(Clone, Copy, Debug, Default)]
pub struct TachyonAdapter;

impl EngineAdapter for TachyonAdapter {
    /// Executes one request without translating Rust panics into ECMAScript outcomes.
    fn execute(&self, request: ExecutionRequest<'_>) -> EngineResponse {
        EngineResponse::new(execute_request(request))
    }
}

/// Parses the body before harness lowering, then executes all source units in one isolate.
fn execute_request(request: ExecutionRequest<'_>) -> EngineOutcome {
    if request.is_async {
        return unsupported("Tachyon async Test262 completion is not implemented");
    }

    let body_mode = if request.is_module {
        SourceMode::Module
    } else {
        SourceMode::Script
    };
    if let Err(error) = Compiler.parse(source_text(0, &request.test.body), options(body_mode)) {
        return body_compile_error(error);
    }
    for (index, prelude) in request.test.preludes.iter().enumerate() {
        if let Err(error) = Compiler.parse(
            source_text(source_id(index, 1), prelude),
            options(SourceMode::Script),
        ) {
            return unsupported(format!(
                "Tachyon cannot parse Test262 harness `{}`: {}",
                prelude.name,
                compile_error_message(&error)
            ));
        }
    }

    let mut isolate = match Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(
            ATOM_MAX_ENTRIES,
            ATOM_MAX_BYTES,
            AtomHashSeed::new(0x7461_6368_796f_6e31, 0x7465_7374_3236_3231),
        ),
        HeapLimit::new(HEAP_LIMIT_BYTES),
        StackLimits::new(STACK_MAX_FRAMES, STACK_MAX_REGISTERS),
        RealmLimits::new(MAX_LOADED_MODULES, MAX_GLOBAL_BINDINGS),
    )) {
        Ok(isolate) => isolate,
        Err(error) => return unsupported(format!("Tachyon isolate creation failed: {error:?}")),
    };
    for (index, prelude) in request.test.preludes.iter().enumerate() {
        let module = match Compiler.compile(
            source_text(source_id(index, 1), prelude),
            options(SourceMode::Script),
        ) {
            Ok(module) => module,
            Err(error) => {
                return unsupported(format!(
                    "Tachyon cannot lower Test262 harness `{}`: {}",
                    prelude.name,
                    compile_error_message(&error)
                ));
            }
        };
        if let Some(outcome) = execute_module(&mut isolate, &module) {
            return outcome;
        }
    }

    let module = match Compiler.compile(source_text(0, &request.test.body), options(body_mode)) {
        Ok(module) => module,
        Err(error) => return body_compile_error(error),
    };
    execute_module(&mut isolate, &module).unwrap_or(EngineOutcome::Completed)
}

fn options(source_mode: SourceMode) -> CompileOptions {
    CompileOptions { source_mode }
}

fn source_id(index: usize, offset: u32) -> u32 {
    u32::try_from(index)
        .ok()
        .and_then(|index| index.checked_add(offset))
        .unwrap_or(u32::MAX)
}

fn source_text(id: u32, unit: &SourceUnit) -> SourceText {
    SourceText::new(
        SourceId::new(id),
        SourceName::new(Arc::<str>::from(&*unit.name)),
        MediaType::JavaScript,
        Arc::clone(&unit.source),
    )
}

/// Maps the current VM outcomes without treating missing exception objects as a successful test.
fn execute_module(
    isolate: &mut Isolate,
    module: &tachyon_bytecode::CompiledModule,
) -> Option<EngineOutcome> {
    let outcome = match isolate.execute(
        module,
        ExecutionBudget {
            fuel: EXECUTION_FUEL_LIMIT,
            quantum: u32::MAX,
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Some(unsupported(format!(
                "Tachyon VM does not support test: {error:?}"
            )));
        }
    };
    match outcome {
        RunOutcome::Completed(_) => None,
        RunOutcome::Thrown(value) => {
            let kind = match isolate.native_error_kind(value) {
                Ok(Some(kind)) => kind,
                Ok(None) => {
                    return Some(EngineOutcome::Error {
                        phase: Phase::Runtime,
                        error_type: "Error".into(),
                        message: format!("Tachyon threw non-native value {value:?}").into(),
                    });
                }
                Err(error) => {
                    return Some(unsupported(format!(
                        "Tachyon could not classify thrown value {value:?}: {error:?}"
                    )));
                }
            };
            Some(EngineOutcome::Error {
                phase: Phase::Runtime,
                error_type: kind.as_str().into(),
                message: format!("Tachyon threw native {}", kind.as_str()).into(),
            })
        }
        RunOutcome::BudgetExhausted => Some(EngineOutcome::Timeout {
            message: format!("Tachyon exhausted the {EXECUTION_FUEL_LIMIT} instruction fuel limit")
                .into(),
        }),
    }
}

fn body_compile_error(error: CompileError) -> EngineOutcome {
    match error {
        CompileError::Diagnostics(diagnostics) => EngineOutcome::Error {
            phase: Phase::Parse,
            error_type: "SyntaxError".into(),
            message: diagnostics
                .first()
                .map_or_else(
                    || "Tachyon parse failed".into(),
                    |item| item.message.clone(),
                )
                .to_string()
                .into(),
        },
        other => unsupported(format!(
            "Tachyon does not support test source: {}",
            compile_error_message(&other)
        )),
    }
}

fn compile_error_message(error: &CompileError) -> String {
    format!("{error:?}")
}

fn unsupported(reason: impl Into<Box<str>>) -> EngineOutcome {
    EngineOutcome::Unsupported {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::TachyonAdapter;
    use crate::{
        ComposedTest, EngineAdapter, EngineOutcome, ExecutionRequest, SourceUnit, TestVariant,
        VariantKind,
    };

    /// Builds one content-independent adapter fixture without touching the checkout or filesystem.
    fn composed(body: &str, preludes: &[(&str, &str)], is_async: bool) -> ComposedTest {
        ComposedTest {
            variant: TestVariant {
                kind: VariantKind::Raw,
                is_async,
                can_block: false,
                use_harness: !preludes.is_empty(),
            },
            preludes: preludes
                .iter()
                .map(|(name, source)| SourceUnit {
                    name: (*name).into(),
                    source: (*source).into(),
                })
                .collect(),
            body: SourceUnit {
                name: "test.js".into(),
                source: body.into(),
            },
            source_sha256: "fixture".into(),
        }
    }

    fn execute(test: &ComposedTest) -> EngineOutcome {
        TachyonAdapter
            .execute(ExecutionRequest {
                test,
                can_block: false,
                is_module: false,
                is_async: test.variant.is_async,
            })
            .outcome
    }

    #[test]
    fn raw_arithmetic_executes_in_tachyon() {
        assert_eq!(
            execute(&composed("1 + 2;", &[], false)),
            EngineOutcome::Completed
        );
    }

    #[test]
    fn body_syntax_errors_keep_the_parse_phase() {
        assert!(matches!(
            execute(&composed("const = ;", &[], false)),
            EngineOutcome::Error { phase: crate::Phase::Parse, ref error_type, .. }
                if &**error_type == "SyntaxError"
        ));
    }

    #[test]
    fn native_runtime_errors_keep_their_ecmascript_type() {
        assert!(matches!(
            execute(&composed("'use strict'; missing = 1;", &[], false)),
            EngineOutcome::Error { phase: crate::Phase::Runtime, ref error_type, .. }
                if &**error_type == "ReferenceError"
        ));
    }

    #[test]
    fn control_flow_harness_executes_in_tachyon() {
        assert_eq!(
            execute(&composed(
                "assert();",
                &[("assert.js", "function assert() { if (true) {} }")],
                false
            )),
            EngineOutcome::Completed
        );
    }

    #[test]
    fn harness_reference_errors_and_unsupported_async_are_explicit() {
        assert!(matches!(
            execute(&composed(
                "1 + 2;",
                &[("assert.js", "assert.value;")],
                false
            )),
            EngineOutcome::Error { phase: crate::Phase::Runtime, ref error_type, .. }
                if &**error_type == "ReferenceError"
        ));
        assert!(matches!(
            execute(&composed("1 + 2;", &[], true)),
            EngineOutcome::Unsupported { .. }
        ));
    }
}
