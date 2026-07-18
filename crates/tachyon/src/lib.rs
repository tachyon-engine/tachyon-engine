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
            HeapLimit::new(8 * SPAN_SIZE_BYTES),
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
                    fuel: 256,
                    quantum: 256,
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
                    fuel: 8,
                    quantum: 8,
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
                    fuel: 8,
                    quantum: 8,
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
                    fuel: 8,
                    quantum: 8,
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
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(tachyon_value::Immediate::Undefined)
        ));
    }

    #[test]
    /// Covers entry hoisting, block escape, duplicate declarations, and parameter var reuse.
    fn var_bindings_follow_function_and_script_scope() {
        assert_eq!(
            execute_source(40, "typeof before === 'undefined'; var before = 3;").as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
        assert_eq!(
            execute_source(41, "{ var escaped = 4; } escaped;").as_i32(),
            Some(4)
        );
        assert_eq!(
            execute_source(42, "var value = 1; var value = value + 2; value;").as_i32(),
            Some(3)
        );
        assert_eq!(
            execute_source(
                43,
                "function preserve(value) { var value; return value; } preserve(8);",
            )
            .as_i32(),
            Some(8)
        );
        assert_eq!(
            execute_source(
                44,
                "function read() { return typeof local; var local = 1; } read() === 'undefined';",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
    }

    #[test]
    /// A later script declaration must not reset an existing global var binding to undefined.
    fn global_var_declaration_preserves_prior_source_unit_value() {
        let first = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(45),
                    SourceName::new("first"),
                    MediaType::JavaScript,
                    Arc::from("var retained = 9;"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let second = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(46),
                    SourceName::new("second"),
                    MediaType::JavaScript,
                    Arc::from("var retained; retained;"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let mut isolate = test_isolate();
        isolate
            .execute(
                &first,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        let outcome = isolate
            .execute(
                &second,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(9)));
    }

    #[test]
    /// Covers simple, compound, update, nested-function, and cross-source resolved global writes.
    fn identifier_assignment_updates_resolved_global_bindings() {
        assert_eq!(
            execute_source(
                50,
                "var value = 1; value = 2; value += 3; value++; ++value; value;",
            )
            .as_i32(),
            Some(7)
        );
        assert_eq!(
            execute_source(
                51,
                "var value = 1; function update() { value += 2; value++; } update(); value;",
            )
            .as_i32(),
            Some(4)
        );

        let first = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(52),
                    SourceName::new("first"),
                    MediaType::JavaScript,
                    Arc::from("var shared = 1;"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let second = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(53),
                    SourceName::new("second"),
                    MediaType::JavaScript,
                    Arc::from("shared = 9; shared;"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let mut isolate = test_isolate();
        isolate
            .execute(
                &first,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        let outcome = isolate
            .execute(
                &second,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(9)));

        let declaration = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(56),
                    SourceName::new("function-declaration"),
                    MediaType::JavaScript,
                    Arc::from("function published() { return 1; }"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let reassignment = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(57),
                    SourceName::new("function-reassignment"),
                    MediaType::JavaScript,
                    Arc::from("published = 11; published;"),
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
                &reassignment,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(11)));
    }

    #[test]
    fn sloppy_intrinsic_assignment_is_ignored_without_creating_a_global() {
        let value = execute_source(54, "Infinity = 1; Infinity;");
        assert_eq!(value.as_f64(), Some(f64::INFINITY));
    }

    #[test]
    fn strict_module_intrinsic_assignment_throws_a_type_error() {
        let module = Compiler
            .compile(
                SourceText::new(
                    SourceId::new(55),
                    SourceName::new("strict.mjs"),
                    MediaType::Mjs,
                    Arc::from("Infinity = 1;"),
                ),
                CompileOptions::default(),
            )
            .unwrap();
        assert!(matches!(
            test_isolate().execute(
                &module,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                }
            ),
            Ok(RunOutcome::Thrown(_))
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
                    fuel: 12,
                    quantum: 12,
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
                    fuel: 12,
                    quantum: 12,
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
                    fuel: 8,
                    quantum: 8,
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
    /// Exercises dispatch order, default placement, fallthrough, and nearest-target break semantics.
    fn switch_preserves_ecmascript_clause_control_flow() {
        assert_eq!(
            execute_source(
                25,
                "let seen = 0; switch (1) { case (seen = seen + 1): break; case (seen = seen + 1): break; } seen;",
            )
            .as_i32(),
            Some(1)
        );
        assert_eq!(
            execute_source(
                26,
                "let value = 0; switch (9) { case 1: value = 1; break; default: value = 3; case 2: value = value + 4; break; } value;",
            )
            .as_i32(),
            Some(7)
        );
        assert_eq!(
            execute_source(
                27,
                "let value = 0; switch (1) { case 1: value = 1; case 2: value = value + 2; break; default: value = 9; } value;",
            )
            .as_i32(),
            Some(3)
        );
        assert_eq!(
            execute_source(
                28,
                "let value = 0; switch (1) { case 1: switch (2) { case 2: value = 3; break; default: value = 9; } value = value + 4; break; default: value = 99; } value;",
            )
            .as_i32(),
            Some(7)
        );
        assert_eq!(
            execute_source(29, "1; switch (9) { default: 2; }").as_i32(),
            Some(2)
        );
        assert_eq!(
            execute_source(30, "1; switch (9) { case 2: 3; }").as_i32(),
            Some(1)
        );
        assert_eq!(
            execute_source(
                31,
                "function select(value) { switch (value) { case 1: return 10; default: return 20; } } select(2);",
            )
            .as_i32(),
            Some(20)
        );
        assert_eq!(
            execute_source(
                32,
                "let key = 1; let hit = 0; switch (key) { case (key = 2): hit = 1; break; default: hit = 3; } hit;",
            )
            .as_i32(),
            Some(3)
        );
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
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Thrown(_)));
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
    /// Covers declarative global visibility, TDZ, immutable assignment, and cross-script redeclaration.
    fn global_lexical_bindings_preserve_ecmascript_state() {
        let compile = |id, name, text| {
            Compiler
                .compile(
                    SourceText::new(
                        SourceId::new(id),
                        SourceName::new(name),
                        MediaType::JavaScript,
                        Arc::from(text),
                    ),
                    CompileOptions::default(),
                )
                .unwrap()
        };
        let setup = compile(
            55,
            "lexical-setup.js",
            "let retained = 41; function read() { return retained + 1; }",
        );
        let body = compile(56, "lexical-body.js", "read();");
        let mut isolate = test_isolate();
        let body_code = isolate.load_module(&body).unwrap();
        isolate
            .execute(
                &setup,
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        let outcome = isolate
            .execute_loaded(
                body_code,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));

        let tdz = compile(57, "tdz.js", "value; let value = 1;");
        assert!(matches!(
            test_isolate().execute(
                &tdz,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16
                }
            ),
            Ok(RunOutcome::Thrown(_))
        ));
        let immutable = compile(58, "const.js", "const value = 1; value = 2;");
        assert!(matches!(
            test_isolate().execute(
                &immutable,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16
                }
            ),
            Ok(RunOutcome::Thrown(_))
        ));
        let redeclaration = compile(59, "redeclaration.js", "let retained = 2;");
        assert!(matches!(
            isolate.execute(
                &redeclaration,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16
                }
            ),
            Ok(RunOutcome::Thrown(_))
        ));
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
        let mut code_limited =
            test_isolate_with_realm_limits(RealmLimits::new(1, 2).with_max_shapes(64));
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

        let mut global_limited =
            test_isolate_with_realm_limits(RealmLimits::new(2, 1).with_max_shapes(64));
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

    #[test]
    fn try_catch_preserves_binding_normal_path_and_nested_completion() {
        assert_eq!(
            execute_source(
                25,
                "let result = 0; try { throw 42; } catch (error) { result = error; } result;",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                26,
                "let result = 1; try { result = 7; } catch (error) { result = 9; } result;",
            )
            .as_i32(),
            Some(7)
        );
        assert_eq!(
            execute_source(27, "try { throw 5; } catch { 8; }").as_i32(),
            Some(8)
        );
        assert_eq!(
            execute_source(
                28,
                "let result = 0; try { try { throw 3; } catch (inner) { throw inner; } } catch (outer) { result = outer; } result;",
            )
            .as_i32(),
            Some(3)
        );
    }

    #[test]
    fn callee_throw_enters_caller_catch_without_native_unwind() {
        let value = execute_source(
            29,
            "function fail() { throw 42; } let result = 0; try { fail(); } catch (error) { result = error; } result;",
        );
        assert_eq!(value.as_i32(), Some(42));
    }

    #[test]
    fn function_expressions_are_callable_and_function_objects_hold_methods() {
        assert_eq!(
            execute_source(
                30,
                "let outer = function () { return function () { return 42; }; }; outer()();",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                31,
                "function assert() {} assert._isSameValue = function (value) { return value + 1; }; assert._isSameValue(41);",
            )
            .as_i32(),
            Some(42)
        );
    }

    #[test]
    fn function_prototype_call_forwards_this_and_positional_arguments() {
        let value = execute_source(
            60,
            "function sum(left, right) { return this + left + right; } sum.call(10, 20, 12);",
        );
        assert_eq!(value.as_i32(), Some(42));
    }

    #[test]
    fn function_strictness_controls_nullish_this_binding() {
        let sloppy = execute_source(
            61,
            "function readThis() { return this; } this === readThis.call(undefined);",
        );
        assert_eq!(sloppy.as_immediate(), Some(tachyon_value::Immediate::True));
        let strict = execute_source(
            62,
            "function readThis() { 'use strict'; return this; } readThis.call(undefined) === undefined;",
        );
        assert_eq!(strict.as_immediate(), Some(tachyon_value::Immediate::True));
    }

    #[test]
    fn strict_reference_failures_are_catchable_native_error_objects() {
        let caught = execute_source(
            63,
            "function fail() { 'use strict'; missing = 1; } try { fail(); } catch (error) { error.constructor === ReferenceError; }",
        );
        assert_eq!(caught.as_immediate(), Some(tachyon_value::Immediate::True));
        let constructed = execute_source(
            64,
            "var called = ReferenceError(); var built = new ReferenceError(); called.constructor === ReferenceError && built instanceof ReferenceError;",
        );
        assert_eq!(
            constructed.as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
    }

    #[test]
    fn ordinary_construct_sets_receiver_new_target_and_return_replacement() {
        assert_eq!(
            execute_source(
                32,
                "function Box(value) { this.value = value; } (new Box(42)).value;",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                33,
                "function Box() { this.value = 42; return 7; } (new Box()).value;",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                34,
                "function replacement() {} function Box() { return replacement; } new Box() === replacement;",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
        assert_eq!(
            execute_source(
                35,
                "function Box() { return new.target; } let constructed = new Box() === Box; let called = Box() === undefined; constructed && called;",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
    }

    #[test]
    /// Exercises observable default function prototypes and constructor-selected receiver chains.
    fn instanceof_uses_the_current_constructor_prototype_chain() {
        assert_eq!(
            execute_source(
                47,
                "function Constructor() {} Constructor.prototype.constructor === Constructor && new Constructor() instanceof Constructor;",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
        assert_eq!(
            execute_source(48, "function Constructor() {} 1 instanceof Constructor;")
                .as_immediate(),
            Some(tachyon_value::Immediate::False)
        );
        assert_eq!(
            execute_source(
                49,
                "function Constructor() {} function Parent() {} Constructor.prototype = Parent.prototype; new Constructor() instanceof Parent;",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
    }

    #[test]
    fn compound_assignment_reads_old_value_before_rhs_and_evaluates_receiver_once() {
        assert_eq!(
            execute_source(36, "let value = 1; value += (value = 2); value;").as_i32(),
            Some(3)
        );
        assert_eq!(
            execute_source(
                37,
                "function Box() { this.value = 1; this.calls = 0; } function target(receiver) { receiver.calls += 1; return receiver; } let box = new Box(); target(box).value += 2; box.calls === 1 && box.value === 3;",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
    }

    #[test]
    fn closure_environment_preserves_mutable_state_across_calls() {
        assert_eq!(
            execute_source(
                51,
                "function outer() { let value = 1; return function() { value += 1; return value; }; } let next = outer(); next(); next();",
            )
            .as_i32(),
            Some(3)
        );
        assert_eq!(
            execute_source(
                52,
                "function outer() { let first = 20; return function() { let second = 22; return function() { return first + second; }; }; } outer()()();",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                53,
                "function outer() { let first = 20; function middle() { let second = 22; function inner() { return first + second; } return inner; } return middle; } outer()()();",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                54,
                "function outer() { return inner(); function inner() { return 42; } } outer();",
            )
            .as_i32(),
            Some(42)
        );
    }

    #[test]
    /// Covers both script completion and ordinary-function loop control paths.
    fn classic_for_loop_runs_update_on_continue_and_exits_on_break() {
        assert_eq!(
            execute_source(
                38,
                "let sum = 0; for (let index = 0; index < 4; index++) { if (index === 2) { continue; } sum += index; if (index === 3) { break; } } sum;",
            )
            .as_i32(),
            Some(4)
        );
        assert_eq!(
            execute_source(
                39,
                "function sumTo(limit) { let sum = 0; for (let index = 0; index < limit; ++index) { sum += index; } return sum; } sumTo(5);",
            )
            .as_i32(),
            Some(10)
        );
    }

    #[test]
    /// Covers pre-test/post-test ordering, continue targets, breaks, and script completion values.
    fn while_and_do_while_preserve_loop_control_and_completion() {
        assert_eq!(
            execute_source(
                55,
                "let sum = 0; let index = 0; while (index < 5) { index++; if (index === 2) continue; sum += index; if (index === 4) break; } sum;",
            )
            .as_i32(),
            Some(8)
        );
        assert_eq!(
            execute_source(56, "do { 42; break; } while (true);").as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                57,
                "function total() { let index = 0; let sum = 0; do { index++; if (index < 3) continue; sum += index; } while (index < 4); return sum; } total();",
            )
            .as_i32(),
            Some(7)
        );
        assert_eq!(
            execute_source(58, "while (false) { 1; }").as_immediate(),
            Some(tachyon_value::Immediate::Undefined)
        );
    }

    #[test]
    /// Exercises object literal creation, ordered data-property publication, and shape-backed updates.
    fn object_literals_publish_and_update_plain_data_properties() {
        assert_eq!(
            execute_source(
                59,
                "let object = { answer: 40, label: 'ok' }; object.answer + 2;",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                60,
                "let object = { answer: 1 }; object.answer += 1; object.answer;",
            )
            .as_i32(),
            Some(2)
        );
    }

    #[test]
    /// Checks computed string keys evaluate before values and use the same ordinary property storage.
    fn object_literals_support_computed_string_keys() {
        assert_eq!(
            execute_source(
                61,
                "let key = 'answer'; let object = { [key]: 40 }; object.answer + 2;",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                62,
                "let calls = 0; let object = { [++calls]: 41 }; object[1] + calls;",
            )
            .as_i32(),
            Some(42)
        );
    }

    #[test]
    /// Covers ordinary and computed object methods through the existing receiver call path.
    fn object_literals_support_methods() {
        assert_eq!(
            execute_source(
                80,
                "let object = { answer() { return 40; } }; object.answer() + 2;"
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                81,
                "let object = { ['answer']() { return 40; } }; object.answer() + 2;",
            )
            .as_i32(),
            Some(42)
        );
    }

    #[test]
    /// Covers omitted arguments, explicit undefined, supplied values, and left-to-right defaults.
    fn default_parameters_use_undefined_only_and_see_prior_parameters() {
        assert_eq!(
            execute_source(
                63,
                "function add(value = 40, next = value + 1) { return next; } add();",
            )
            .as_i32(),
            Some(41)
        );
        assert_eq!(
            execute_source(
                64,
                "function add(value = 40, next = value + 1) { return next; } add(undefined);",
            )
            .as_i32(),
            Some(41)
        );
        assert!(
            execute_source(
                65,
                "function add(value = 40, next = value + 1) { return next; } add(null);",
            )
            .as_f64()
            .is_some_and(f64::is_nan)
        );
        assert_eq!(
            execute_source(
                66,
                "function add(value = 40, next = value + 1) { return next; } add(10);",
            )
            .as_i32(),
            Some(11)
        );
    }

    #[test]
    /// Checks update results and one-shot object/key evaluation through source compilation.
    fn computed_members_preserve_reference_evaluation_and_updates() {
        assert_eq!(
            execute_source(
                40,
                "function Box() { this[0] = 40; } let box = new Box(); box[0]++; box[0] += 1; box[0];",
            )
            .as_i32(),
            Some(42)
        );
        assert_eq!(
            execute_source(
                41,
                "function Box() { this[0] = 1; this.calls = 0; } function target(receiver) { receiver.calls += 1; return receiver; } function key(receiver) { receiver.calls += 1; return 0; } let box = new Box(); target(box)[key(box)] += 2; box.calls === 2 && box[0] === 3;",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
    }

    #[test]
    /// Exercises observable string primitives instead of inspecting internal GC descriptors.
    fn typeof_and_string_constants_follow_primitive_semantics() {
        assert_eq!(
            execute_source(
                42,
                "typeof undefined === 'undefined' && typeof null === 'object' && typeof true === 'boolean' && typeof 1 === 'number' && typeof 'x' === 'string';",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
        assert_eq!(
            execute_source(
                43,
                "function Box() {} typeof Box === 'function' && typeof new Box() === 'object' && 'same' === 'same' && !'';",
            )
            .as_immediate(),
            Some(tachyon_value::Immediate::True)
        );
    }
}
