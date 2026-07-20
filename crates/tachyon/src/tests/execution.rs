use super::*;

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
fn arrow_function_expression_can_be_called() {
    assert_eq!(
        execute_source(147, "let add = (left, right) => left + right; add(2, 3);").as_i32(),
        Some(5)
    );
}

#[test]
/// Covers computed accessor key coercion, property publication, and the accessor naming contract.
fn computed_object_accessors_preserve_runtime_key_order_and_names() {
    assert_eq!(
        execute_source(
            158,
            "let key = 'value'; let object = { get [key]() { return 40; }, set [key](next) {} }; let descriptor = Object.getOwnPropertyDescriptor(object, key); object[key] === 40 && descriptor.get.name === 'get value' && descriptor.set.name === 'set value';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            159,
            "let key = Symbol('item'); let object = { get [key]() { return 7; } }; let descriptor = Object.getOwnPropertyDescriptor(object, key); object[key] === 7 && descriptor.get.name === 'get [item]';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
fn sequence_expression_preserves_side_effects_and_last_value() {
    assert_eq!(
        execute_source(
            155,
            "let value = 0; let result = (value = 1, value += 2, value * 10); result + value;",
        )
        .as_i32(),
        Some(33)
    );
    assert_eq!(
        execute_source(156, "let choose = () => (1, 2, 42); choose();").as_i32(),
        Some(42)
    );
}

#[test]
fn array_sort_default_comparator_orders_strings_and_holes() {
    assert_eq!(
        execute_source(
            157,
            "let values = [10, 2, 'a', undefined, , 1]; values.sort() === values && values[0] === 1 && values[1] === 10 && values[2] === 2 && values[3] === 'a' && values[4] === undefined && values.length === 6;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
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
/// Proves a function expression stored on a global callable receives multiple later-unit args.
fn function_property_arguments_survive_across_source_units() {
    let compiler = Compiler;
    let publisher = compiler
        .compile(
            SourceText::new(
                SourceId::new(161),
                SourceName::new("harness.js"),
                MediaType::JavaScript,
                Arc::from(
                    "function holder() {} holder.equal = function (left, right) { if (left === right) { return left !== 0 || 1 / left === 1 / right; } return left !== left && right !== right; };",
                ),
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let caller = compiler
        .compile(
            SourceText::new(
                SourceId::new(162),
                SourceName::new("body.js"),
                MediaType::JavaScript,
                Arc::from("holder.equal('0', '0');"),
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let mut isolate = test_isolate();
    isolate
        .execute(
            &publisher,
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    let outcome = isolate
        .execute(
            &caller,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(tachyon_value::Immediate::True)
        ),
        "unexpected cross-source outcome: {outcome:?}"
    );
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
        test_isolate_with_realm_limits(RealmLimits::new(1, 2).with_max_shapes(256));
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
        test_isolate_with_realm_limits(RealmLimits::new(2, 1).with_max_shapes(256));
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
/// Covers the synchronous object-pattern batch across declaration and assignment write modes.
fn object_destructuring_preserves_nested_defaults_and_coercibility() {
    assert_eq!(
        execute_source(
            905,
            "let { a, nested: { b }, missing = 4, ['c']: c } = { a: 1, nested: { b: 2 }, c: 3 }; a + b + c + missing;",
        )
        .as_i32(),
        Some(10)
    );
    assert_eq!(
        execute_source(
            906,
            "let a = 0; let b = 0; ({ a, x: { b } } = { a: 5, x: { b: 7 } }); a + b;",
        )
        .as_i32(),
        Some(12)
    );
    assert_eq!(
        execute_source(907, "var { a, b = 3 } = { a: 2 }; a + b;").as_i32(),
        Some(5)
    );
    assert_eq!(
        execute_source(
            908,
            "let caught = false; try { const {} = null; } catch (error) { caught = error instanceof TypeError; } caught;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Covers object-rest declaration, var initialization, assignment, and computed exclusion keys.
fn object_destructuring_rest_copies_only_non_excluded_own_properties() {
    assert_eq!(
        execute_source(
            913,
            "let key = 'a'; let { [key]: first, ...tail } = { a: 1, b: 2, c: 3 }; first + tail.b + tail.c;",
        )
        .as_i32(),
        Some(6)
    );
    assert_eq!(
        execute_source(
            914,
            "var rest; var { a, ...rest } = { a: 1, b: 4 }; a + rest.b;",
        )
        .as_i32(),
        Some(5)
    );
    assert_eq!(
        execute_source(
            915,
            "let a = 0; let rest = {}; ({ a, ...rest } = { a: 2, b: 5 }); a + rest.b;",
        )
        .as_i32(),
        Some(7)
    );
    assert_eq!(
        execute_source(
            916,
            "let calls = 0; let source = { get a() { calls++; return 3; }, b: 4 }; let { ...rest } = source; rest.a + rest.b + calls;",
        )
        .as_i32(),
        Some(8)
    );
    assert_eq!(
        execute_source(
            917,
            "let calls = 0; let source = { get a() { calls++; return 3; }, b: 4 }; let { a, ...rest } = source; a + rest.b + calls;",
        )
        .as_i32(),
        Some(8)
    );
}

#[test]
/// Covers a custom `Symbol.iterator` record and the normal early-close branch.
fn array_destructuring_uses_symbol_iterator_and_closes_early() {
    assert_eq!(
        execute_source(
            910,
            "let stage = 0; let iter = {}; stage = 1; iter[Symbol.iterator] = function() { stage = 2; return { next: function() { stage = 3; return { value: 1, done: false }; }, return: function() { stage = 4; return {}; } }; }; let [first] = iter; if (stage === 4) stage = 6; stage;"
        )
        .as_i32(),
        Some(6)
    );
}

#[test]
fn array_destructuring_reads_values_in_iterator_order() {
    assert_eq!(
        execute_source(
            911,
            "let [first, second = 4] = [1, 2]; first * 10 + second;"
        )
        .as_i32(),
        Some(12)
    );
}

#[test]
/// Collects the remaining values from the shared synchronous iterator into a fresh array.
fn array_destructuring_rest_collects_the_iterator_tail() {
    assert_eq!(
        execute_source(
            208,
            "let [first, ...rest] = [1, 2, 3]; first + rest[0] + rest[1];"
        )
        .as_i32(),
        Some(6),
    );
    assert_eq!(
        execute_source(
            209,
            "var head; var tail; [head, ...tail] = [1, 2, 3]; head + tail[0] + tail[1];"
        )
        .as_i32(),
        Some(6),
    );
    assert_eq!(
        execute_source(
            210,
            "let head = 0; let tail = []; [head, ...tail] = [1, 2, 3]; head + tail[0] + tail[1];"
        )
        .as_i32(),
        Some(6),
    );
}

#[test]
/// Assigns inferred names to anonymous function defaults without changing ordinary name writes.
fn destructuring_defaults_infer_anonymous_function_names() {
    assert_eq!(
        execute_source_with_heap(
            912,
            "let [arrayName = function() {}] = []; arrayName.name === 'arrayName';",
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}
