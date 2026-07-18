#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Stable Rust facade for embedding the Tachyon ECMAScript engine.
//!
//! Hosts provide source bytes, module loading, clocks, entropy, and event-loop integration.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
    use tachyon_gc::{HeapLimit, SPAN_SIZE_BYTES};
    use tachyon_vm::{
        AtomHashSeed, AtomTableConfig, ExecutionBudget, Isolate, IsolateConfig, RunOutcome,
        StackLimits,
    };

    fn test_isolate() -> Isolate {
        Isolate::new(IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(4 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
        ))
        .expect("test isolate descriptors register")
    }

    #[test]
    fn source_to_verified_module_to_int32_result() {
        let source = SourceText::new(
            SourceId::new(0),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("1 + 2;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    #[test]
    fn source_to_verified_module_to_number_result() {
        let source = SourceText::new(
            SourceId::new(1),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("1.5 + 2.5;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_f64() == Some(4.0)));
    }

    #[test]
    fn source_to_verified_module_with_local_binding() {
        let source = SourceText::new(
            SourceId::new(2),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("let x = 1; x + 2;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    #[test]
    fn declaration_only_script_returns_undefined() {
        let source = SourceText::new(
            SourceId::new(3),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("let x = 1;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 3,
                    quantum: 3,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(tachyon_value::Immediate::Undefined)
        ));
    }

    #[test]
    fn later_declarator_reads_preceding_local_binding() {
        let source = SourceText::new(
            SourceId::new(4),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("const one = 1, two = one + 2; two * 3;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(9)));
    }

    #[test]
    fn assignment_updates_an_initialized_or_uninitialized_let_binding() {
        let source = SourceText::new(
            SourceId::new(5),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("let x; x = 1; x = x + 2;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 7,
                    quantum: 7,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    #[test]
    fn boolean_and_null_literals_remain_non_numeric_immediates() {
        let source = SourceText::new(
            SourceId::new(6),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("true === false;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(tachyon_value::Immediate::False)
        ));

        let source = SourceText::new(
            SourceId::new(7),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("null === null;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(tachyon_value::Immediate::True)
        ));
    }

    #[test]
    fn conditional_expression_branches_without_executing_the_alternate_arm() {
        let source = SourceText::new(
            SourceId::new(8),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("true ? 1 : 2;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(1)));

        let source = SourceText::new(
            SourceId::new(9),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("0 ? 1 : 2;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 5,
                    quantum: 5,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
    }

    #[test]
    fn source_to_hoisted_function_call_uses_explicit_vm_frames() {
        let source = SourceText::new(
            SourceId::new(10),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("addTwo(40); function addTwo(value) { return value + 2; }"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    fn logical_not_uses_the_shared_truthiness_contract() {
        let source = SourceText::new(
            SourceId::new(10),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("!0;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 3,
                    quantum: 3,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(tachyon_value::Immediate::True)
        ));
    }
}
