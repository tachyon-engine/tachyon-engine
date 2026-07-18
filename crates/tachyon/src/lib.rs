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
        AtomHashSeed, AtomTableConfig, ExecutionBudget, ExecutionError, Isolate, IsolateConfig,
        RealmLimits, RunOutcome, StackLimits,
    };

    fn test_isolate() -> Isolate {
        test_isolate_with_realm_limits(RealmLimits::new(64, 1_024))
    }

    fn test_isolate_with_realm_limits(realm_limits: RealmLimits) -> Isolate {
        Isolate::new(IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(4 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            realm_limits,
        ))
        .expect("test isolate descriptors register")
    }

    /// Compiles and executes one in-memory script with enough budget for expression fixtures.
    fn execute_source(source_id: u32, text: &str) -> tachyon_value::Value {
        let module = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(source_id),
                    SourceName::new("embedded-input"),
                    MediaType::JavaScript,
                    Arc::from(text),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        match test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 64,
                    quantum: 64,
                },
            )
            .unwrap()
        {
            RunOutcome::Completed(value) => value,
            outcome => panic!("expression fixture did not complete: {outcome:?}"),
        }
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
    /// Exercises UpdateEmpty-style script completion through nested lexical blocks and branches.
    fn top_level_block_and_if_preserve_script_completion() {
        let source = SourceText::new(
            SourceId::new(11),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("1; if (false) { 2; } else { { let local = 3; local; } }"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    #[test]
    /// Exercises source-level branch lowering inside an explicit JavaScript call frame.
    fn function_if_selects_return_without_rust_recursion() {
        let source = SourceText::new(
            SourceId::new(12),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("function select(flag) { if (flag) { return 1; } return 2; } select(false);"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
    }

    #[test]
    /// Confirms source-level throw exits through the VM outcome instead of Rust unwinding.
    fn callee_throw_becomes_an_explicit_vm_outcome() {
        let source = SourceText::new(
            SourceId::new(13),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("function fail() { throw 7; } fail();"),
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
        assert!(matches!(outcome, RunOutcome::Thrown(value) if value.as_i32() == Some(7)));
    }

    #[test]
    /// Confirms a block binding falls through to runtime global resolution after its checkpoint.
    fn block_lexical_binding_is_not_visible_after_the_block() {
        let source = SourceText::new(
            SourceId::new(14),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("function scoped() { { let hidden = 1; } return hidden; } scoped();"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let error = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap_err();
        assert!(matches!(error, ExecutionError::UnresolvedBinding(_)));
    }

    #[test]
    /// Proves a closure published by one script retains its own code when called by a later script.
    fn global_function_binding_survives_across_source_units() {
        let declaration = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(15),
                    SourceName::new("harness.js"),
                    MediaType::JavaScript,
                    Arc::from("function addTwo(value) { return value + 2; }"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let body = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(16),
                    SourceName::new("body.js"),
                    MediaType::JavaScript,
                    Arc::from("addTwo(40);"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let mut isolate = test_isolate();
        isolate
            .execute(
                &declaration,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        let outcome = isolate
            .execute(
                &body,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    /// Confirms repeated code is reused while distinct modules and globals obey independent hard limits.
    fn loaded_code_and_global_binding_limits_are_explicit() {
        let first = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(17),
                    SourceName::new("first.js"),
                    MediaType::JavaScript,
                    Arc::from("function first() {}"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let second = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(18),
                    SourceName::new("second.js"),
                    MediaType::JavaScript,
                    Arc::from("function second() {}"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let mut code_limited = test_isolate_with_realm_limits(RealmLimits::new(1, 2));
        for _ in 0..2 {
            code_limited
                .execute(
                    &first,
                    ExecutionBudget {
                        fuel: 8,
                        quantum: 8,
                    },
                )
                .unwrap();
        }
        assert!(matches!(
            code_limited.execute(
                &second,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                }
            ),
            Err(ExecutionError::LoadedModuleLimit { limit: 1 })
        ));

        let mut global_limited = test_isolate_with_realm_limits(RealmLimits::new(2, 1));
        global_limited
            .execute(
                &first,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(
            global_limited.execute(
                &second,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                }
            ),
            Err(ExecutionError::GlobalBindingLimit { limit: 1 })
        ));
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

    #[test]
    fn logical_expressions_preserve_values_and_skip_right_hand_side_effects() {
        assert_eq!(execute_source(19, "0 || 7;").as_i32(), Some(7));
        assert_eq!(execute_source(20, "5 && 7;").as_i32(), Some(7));
        assert_eq!(execute_source(21, "null ?? 9;").as_i32(), Some(9));
        assert_eq!(execute_source(22, "4 ?? 9;").as_i32(), Some(4));
        assert_eq!(
            execute_source(
                23,
                "let changed = 0; false && (changed = 1); true || (changed = 2); changed;",
            )
            .as_i32(),
            Some(0)
        );
    }

    #[test]
    fn numeric_negation_and_realm_infinity_preserve_ieee_zero_sign() {
        let value = execute_source(24, "1 / 0 === Infinity && 1 / -0 === -Infinity;");
        assert_eq!(value.as_immediate(), Some(tachyon_value::Immediate::True));
    }
}
