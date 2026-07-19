use super::*;

#[test]
/// Confirms control-flow HIR owns every nested node after the Oxc arena is dropped.
fn hir_owns_nested_block_if_and_throw_statements() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "if (false) { let hidden = 1; } else { throw 7; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [statement] = hir.statements() else {
        panic!("expected one conditional statement");
    };
    let HirStatementKind::If {
        consequent,
        alternate: Some(alternate),
        ..
    } = &statement.kind
    else {
        panic!("expected owned conditional arms");
    };
    assert!(matches!(consequent.kind, HirStatementKind::Block(_)));
    assert!(matches!(alternate.kind, HirStatementKind::Block(_)));
    let HirStatementKind::Block(statements) = &alternate.kind else {
        unreachable!();
    };
    assert!(matches!(statements[0].kind, HirStatementKind::Throw(_)));
}

#[test]
/// Confirms switch clause order, default placement, and break survive arena teardown.
fn hir_owns_switch_cases_in_source_order() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "switch (2) { case 1: break; default: 3; case 2: 4; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [statement] = hir.statements() else {
        panic!("expected one switch statement");
    };
    let HirStatementKind::Switch {
        discriminant,
        cases,
    } = &statement.kind
    else {
        panic!("expected owned switch HIR");
    };
    assert_eq!(
        discriminant.kind,
        HirExpressionKind::Number(2.0_f64.to_bits())
    );
    assert_eq!(cases.len(), 3);
    assert!(cases[0].test.is_some());
    assert!(matches!(
        cases[0].consequent[0].kind,
        HirStatementKind::Break
    ));
    assert!(cases[1].test.is_none());
    assert!(cases[2].test.is_some());
}

#[test]
/// Keeps pre-test and post-test loop ordering explicit after the Oxc arena is dropped.
fn hir_distinguishes_while_and_do_while_evaluation_order() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "while (false) { 1; } do { 2; } while (false);",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [while_statement, do_while_statement] = hir.statements() else {
        panic!("expected two loop statements");
    };
    assert!(matches!(
        while_statement.kind,
        HirStatementKind::Loop {
            test_first: true,
            ..
        }
    ));
    assert!(matches!(
        do_while_statement.kind,
        HirStatementKind::Loop {
            test_first: false,
            ..
        }
    ));
}

#[test]
/// Owns declaration and assignment heads after Oxc's arena is dropped.
fn hir_owns_for_in_heads_and_body() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "let target; for (const key in { first: 1 }) { target = key; } for (target in {}) {}",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [_, declaration_loop, assignment_loop] = hir.statements() else {
        panic!("expected declaration and two for-in statements");
    };
    assert!(matches!(
        declaration_loop.kind,
        HirStatementKind::ForIn {
            left: crate::hir::HirForInLeft::Variable(_),
            ..
        }
    ));
    assert!(matches!(
        assignment_loop.kind,
        HirStatementKind::ForIn {
            left: crate::hir::HirForInLeft::Assignment(_),
            ..
        }
    ));
}

#[test]
fn compiler_emits_verified_for_in_iterator_bytecode() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "let result; for (let key in { first: 1 }) { result = key; } result;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("CreateForInIterator"));
    assert!(disassembly.contains("ForInNext"));
    assert!(disassembly.contains("JumpIfTrue"));
}

#[test]
/// Owns ordinary object data properties and rejects descriptor-bearing syntax at the boundary.
fn hir_owns_plain_object_literal_properties() {
    let hir = Compiler
        .lower_to_hir(
            source(MediaType::JavaScript, "({ answer: 40, label: 'ok' });"),
            CompileOptions::default(),
        )
        .unwrap();
    let Some(HirStatement {
        kind:
            HirStatementKind::Expression(HirExpression {
                kind: HirExpressionKind::Object(properties),
                ..
            }),
        ..
    }) = hir.statements().first()
    else {
        panic!("expected owned object literal");
    };
    assert_eq!(properties.len(), 2);
    assert!(matches!(
        &properties[0].key,
        HirObjectPropertyKey::Static(key) if key.as_ref() == "answer"
    ));
    assert!(matches!(
        &properties[1].key,
        HirObjectPropertyKey::Static(key) if key.as_ref() == "label"
    ));
    let computed = Compiler
        .lower_to_hir(
            source(MediaType::JavaScript, "({ ['answer']: 40 });"),
            CompileOptions::default(),
        )
        .unwrap();
    assert!(matches!(
        computed.statements().first(),
        Some(HirStatement {
            kind: HirStatementKind::Expression(HirExpression {
                kind: HirExpressionKind::Object(properties),
                ..
            }),
            ..
        }) if matches!(properties[0].key, HirObjectPropertyKey::Computed(_))
    ));
    assert!(matches!(
        Compiler
            .lower_to_hir(
                source(MediaType::JavaScript, "({ ...other });"),
                CompileOptions::default(),
            )
            .unwrap()
            .statements()
            .first(),
        Some(HirStatement {
            kind: HirStatementKind::Expression(HirExpression {
                kind: HirExpressionKind::Call { .. },
                ..
            }),
            ..
        })
    ));
}

#[test]
/// Confirms structured entry lowering produces verifier-accepted branch and abrupt opcodes.
fn compiler_emits_verified_top_level_branch_and_throw() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "if (false) { 1; } else { throw 7; }"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("JumpIfFalse"));
    assert!(disassembly.contains("Throw"));
}

#[test]
fn compiler_emits_each_logical_short_circuit_branch() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "0 && 1; 0 || 2; null ?? 3;"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("JumpIfFalse"));
    assert!(disassembly.contains("JumpIfTrue"));
    assert!(disassembly.contains("JumpIfNotNullish"));
}

#[test]
fn compiler_emits_verified_switch_dispatch_and_break_targets() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "switch (2) { case 1: break; default: 3; case 2: break; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(disassembly.matches("StrictEqual").count(), 2);
    assert_eq!(disassembly.matches("JumpIfTrue").count(), 2);
    assert!(disassembly.matches("Jump").count() >= 4);
}

#[test]
fn compiler_marks_global_lexical_const_as_immutable() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "const answer = 42; answer = 0;"),
            CompileOptions::default(),
        )
        .unwrap();
    let entry = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert!(entry.binding_plan().iter().any(|binding| {
        binding.name.as_ref() == "answer"
            && binding.location == tachyon_bytecode::BindingLocation::GlobalLexical
            && !binding.mutable
    }));
}

#[test]
fn compiler_freezes_directive_and_inherited_function_strictness() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function sloppy() {} function strict() { 'use strict'; function nested() {} }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Sloppy
    );
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Sloppy
    );
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(2))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Strict
    );
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(3))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Strict
    );
}

#[test]
fn unused_parameters_remain_reserved_in_the_verified_frame_layout() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "function pair(first, second) {}"),
            CompileOptions::default(),
        )
        .unwrap();
    let layout = module
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap()
        .layout();
    assert_eq!(layout.argument_count, 2);
    assert_eq!(layout.function_length, 2);
    assert_eq!(layout.register_count, 2);
}

#[test]
fn compiler_lowers_compound_assignment_through_the_shared_binary_path() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "let value = 1; value += 2;"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("Add"));
}

#[test]
fn compiler_freezes_nested_try_catch_ranges_and_exact_depth() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "try { try { throw 1; } catch (inner) { throw inner; } } catch (outer) { outer; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert_eq!(function.handlers().len(), 2);
    assert_eq!(function.layout().max_handler_depth, 2);
    let outer = function.handlers()[0];
    let inner = function.handlers()[1];
    assert!(outer.protected_start.index() < inner.protected_start.index());
    assert!(inner.protected_end.index() < outer.protected_end.index());
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert_eq!(disassembly.matches("LoadException").count(), 2);
}

#[test]
/// Locks the single-body finalizer shape and exact completion-depth metadata.
fn compiler_lowers_finally_to_explicit_completion_replay() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "try { 1; } finally { 2; }"),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert_eq!(function.handlers().len(), 1);
    assert_eq!(
        function.handlers()[0].kind,
        tachyon_bytecode::HandlerKind::Finally
    );
    assert_eq!(function.layout().max_handler_depth, 1);
    assert_eq!(function.layout().max_completion_depth, 1);
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert_eq!(disassembly.matches("EnterFinally").count(), 1);
    assert_eq!(disassembly.matches("ResumeCompletion").count(), 1);
}

#[test]
/// Proves catch remains inner to finally and escaping loop control uses an abrupt target.
fn compiler_lowers_catch_finally_and_break_without_copying_the_finalizer() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "while (true) { try { throw 1; } catch (error) { break; } finally { 2; } }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert_eq!(function.handlers().len(), 2);
    assert_eq!(
        function.handlers()[0].kind,
        tachyon_bytecode::HandlerKind::Finally
    );
    assert_eq!(
        function.handlers()[1].kind,
        tachyon_bytecode::HandlerKind::Catch
    );
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert_eq!(disassembly.matches("BreakThroughFinally").count(), 1);
    assert_eq!(disassembly.matches("ResumeCompletion").count(), 1);
}

#[test]
fn compiler_preserves_static_member_receiver_and_property_names() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "let object; object.value = 1; object.method(2);",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("SetById"));
    assert!(disassembly.contains("GetById"));
    assert!(disassembly.contains("CallWithReceiver"));
    assert!(
        module
            .scope_names()
            .iter()
            .any(|name| name.as_ref() == "value")
    );
    assert!(
        module
            .scope_names()
            .iter()
            .any(|name| name.as_ref() == "method")
    );
}

#[test]
fn compiler_owns_anonymous_and_nested_function_expression_stencils() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "let outer = function () { return function () { return 42; }; }; outer()();",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(hir.functions().len(), 2);
    assert!(
        hir.functions()
            .iter()
            .all(|function| function.name.is_none())
    );
    assert_eq!(hir.functions()[0].id.index(), 0);
    assert_eq!(hir.functions()[1].id.index(), 1);
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "let outer = function () { return function () { return 42; }; }; outer()();",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(module.functions().len(), 3);
}

proptest! {
    #[test]
    fn arbitrary_utf8_input_never_escapes_the_frontend(
        characters in proptest::collection::vec(any::<char>(), 0..128),
    ) {
        let text: String = characters.into_iter().collect();
        let _ = Compiler.parse(
            source(MediaType::JavaScript, &text),
            CompileOptions::default(),
        );
    }
}
