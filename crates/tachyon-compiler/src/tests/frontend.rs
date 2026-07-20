use super::*;

#[test]
fn parser_copies_owned_information_before_dropping_oxc_arena() {
    let parsed = Compiler
        .parse(
            source(MediaType::TypeScript, "const answer: number = 42;"),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(parsed.source().name().as_str(), "embedded-input");
    assert_eq!(
        parsed.top_level_spans(),
        &[SourceSpan { start: 0, end: 26 }]
    );
    assert!(matches!(parsed.kind(), ProgramKind::Script));
    assert!(parsed.diagnostics().is_empty());
}

#[test]
fn parser_supports_jsx_and_module_media_types() {
    let jsx = Compiler
        .parse(
            source(MediaType::Jsx, "const view = <main />;"),
            CompileOptions::default(),
        )
        .unwrap();
    assert!(matches!(jsx.kind(), ProgramKind::Module));
    let module = Compiler
        .parse(
            source(MediaType::Mts, "export const value: number = 1;"),
            CompileOptions::default(),
        )
        .unwrap();
    assert!(matches!(module.kind(), ProgramKind::Module));
}

#[test]
fn parser_returns_owned_diagnostics_for_invalid_source() {
    let error = Compiler
        .parse(
            source(MediaType::JavaScript, "const = ;"),
            CompileOptions::default(),
        )
        .unwrap_err();
    let CompileError::Diagnostics(diagnostics) = error else {
        panic!("invalid JavaScript must report parser diagnostics");
    };
    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics[0].source_name.as_str(), "embedded-input");
    assert!(matches!(diagnostics[0].severity, DiagnosticSeverity::Error));
}

#[test]
fn parser_honors_script_mode_for_module_syntax() {
    assert!(matches!(
        Compiler.parse(
            source(MediaType::JavaScript, "export {};"),
            CompileOptions {
                source_mode: SourceMode::Script,
            },
        ),
        Err(CompileError::Diagnostics(_))
    ));
}

#[test]
fn hir_lowering_copies_binary_expression_without_oxc_values() {
    let hir = Compiler
        .lower_to_hir(
            source(MediaType::JavaScript, "1 + 2;"),
            CompileOptions::default(),
        )
        .unwrap();
    let [statement] = hir.statements() else {
        panic!("expected one HIR statement");
    };
    let HirStatementKind::Expression(HirExpression {
        kind:
            HirExpressionKind::Binary {
                operator: HirBinaryOperator::Add,
                left,
                right,
            },
        ..
    }) = &statement.kind
    else {
        panic!("expected owned binary expression");
    };
    assert_eq!(left.kind, HirExpressionKind::Number(1.0_f64.to_bits()));
    assert_eq!(right.kind, HirExpressionKind::Number(2.0_f64.to_bits()));
    assert_eq!(statement.completion, StatementCompletion::Value);
}

#[test]
/// Confirms logical structure and operator identity survive after the Oxc arena is dropped.
fn hir_lowering_owns_logical_expression_and_operator() {
    let hir = Compiler
        .lower_to_hir(
            source(MediaType::JavaScript, "0 && 2;"),
            CompileOptions::default(),
        )
        .unwrap();
    let [statement] = hir.statements() else {
        panic!("expected one HIR statement");
    };
    assert!(matches!(
        &statement.kind,
        HirStatementKind::Expression(HirExpression {
            kind: HirExpressionKind::Logical {
                operator: HirLogicalOperator::And,
                left,
                right,
            },
            ..
        }) if left.kind == HirExpressionKind::Number(0.0_f64.to_bits())
            && right.kind == HirExpressionKind::Number(2.0_f64.to_bits())
    ));
}

#[test]
fn hir_lowering_copies_local_binding_without_oxc_values() {
    let hir = Compiler
        .lower_to_hir(
            source(MediaType::JavaScript, "let answer = 42;"),
            CompileOptions::default(),
        )
        .unwrap();
    let [statement] = hir.statements() else {
        panic!("expected one HIR statement");
    };
    let HirStatementKind::VariableDeclaration(declaration) = &statement.kind else {
        panic!("expected owned variable declaration");
    };
    assert_eq!(declaration.kind, HirVariableDeclarationKind::Let);
    let [declarator] = declaration.declarators.as_ref() else {
        panic!("expected one owned declarator");
    };
    let HirPattern {
        kind: HirPatternKind::Binding(binding),
        ..
    } = &declarator.pattern
    else {
        panic!("expected simple binding pattern");
    };
    assert_eq!(binding.name.as_ref(), "answer");
    assert!(matches!(
        declarator.initializer.as_ref(),
        Some(HirExpression {
            kind: HirExpressionKind::Number(bits),
            ..
        }) if *bits == 42.0_f64.to_bits()
    ));
}

#[test]
/// Proves same-spelling references retain distinct semantic bindings across nested scopes.
fn hir_owns_scope_binding_and_reference_identity() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "let value = 1; { let value = 2; value; } value;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [outer_declaration, block, outer_read] = hir.statements() else {
        panic!("expected declaration, block, and read");
    };
    let HirStatementKind::VariableDeclaration(outer_declaration) = &outer_declaration.kind else {
        panic!("expected outer declaration");
    };
    let HirStatementKind::Block(block) = &block.kind else {
        panic!("expected nested block");
    };
    let HirStatementKind::VariableDeclaration(inner_declaration) = &block[0].kind else {
        panic!("expected inner declaration");
    };
    let HirStatementKind::Expression(HirExpression {
        kind: HirExpressionKind::Identifier(inner_read),
        ..
    }) = &block[1].kind
    else {
        panic!("expected inner read");
    };
    let HirStatementKind::Expression(HirExpression {
        kind: HirExpressionKind::Identifier(outer_read),
        ..
    }) = &outer_read.kind
    else {
        panic!("expected outer read");
    };
    let HirPattern {
        kind: HirPatternKind::Binding(outer),
        ..
    } = &outer_declaration.declarators[0].pattern
    else {
        panic!("expected outer binding pattern");
    };
    let HirPattern {
        kind: HirPatternKind::Binding(inner),
        ..
    } = &inner_declaration.declarators[0].pattern
    else {
        panic!("expected inner binding pattern");
    };
    assert_ne!(outer.id, inner.id);
    assert_ne!(outer.scope, inner.scope);
    assert_eq!(inner_read.binding, Some(inner.id));
    assert_eq!(outer_read.binding, Some(outer.id));
    assert!(inner_read.read && !inner_read.write);
    assert_eq!(
        hir.scopes()[inner.scope.index() as usize].parent,
        Some(outer.scope)
    );
}

#[test]
fn hir_copies_direct_eval_scope_capability() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "function run() { eval('var value = 1;'); }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = &hir.functions()[0];
    assert!(
        hir.scopes()[function.scope.index() as usize]
            .flags
            .direct_eval
    );
}

#[test]
fn compiler_emits_owned_and_inherited_environment_binding_plans() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function outer() { let value = 1; return function() { return value; }; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let inner = module
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap();
    let outer = module
        .function(tachyon_bytecode::FunctionId::new(2))
        .unwrap();
    assert_eq!(outer.layout().environment_slot_count, 1);
    assert_eq!(inner.layout().environment_slot_count, 0);
    assert!(outer.binding_plan().iter().any(|binding| matches!(
        binding.location,
        tachyon_bytecode::BindingLocation::Environment { depth: 0, slot: 0 }
    )));
    assert!(inner.binding_plan().iter().any(|binding| matches!(
        binding.location,
        tachyon_bytecode::BindingLocation::Environment { depth: 0, slot: 0 }
    )));
    let outer_bytecode = tachyon_bytecode::disassemble(outer).unwrap();
    let inner_bytecode = tachyon_bytecode::disassemble(inner).unwrap();
    assert!(outer_bytecode.contains("StoreEnvironment r0, depth=0, slot=0"));
    assert!(inner_bytecode.contains("LoadEnvironment r0, depth=0, slot=0"));
}

#[test]
/// Freezes exact owner states into storage reserved from the captured-slot count.
fn compiler_freezes_captured_slot_state_and_record_metadata() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function outer(param) { var hoisted; let lexical = 1; const fixed = 2; function declared() {} return function() { declared(); return param + hoisted + lexical + fixed; }; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let owner = module
        .functions()
        .iter()
        .find(|function| function.layout().environment_slot_count == 5)
        .expect("outer owns every referenced capture");
    assert_eq!(
        owner.environment_record_kind(),
        tachyon_bytecode::EnvironmentRecordKind::Function
    );
    let slots = owner
        .environment_slots()
        .iter()
        .enumerate()
        .map(|(slot, metadata)| {
            (
                slot,
                metadata.name.as_ref(),
                metadata.mutable,
                metadata.initialized,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        slots,
        vec![
            (0_usize, "param", true, true),
            (1, "hoisted", true, true),
            (2, "lexical", true, false),
            (3, "fixed", false, false),
            (4, "declared", true, true),
        ]
    );
}

#[test]
/// Confirms compressed environment chains remain explicit in direct opcode operands.
fn compiler_emits_nonzero_environment_depth_without_runtime_plan_lookup() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function outer() { let x = 1; return function() { let y = 2; return function() { return x + y; }; }; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = module
        .functions()
        .iter()
        .map(|function| tachyon_bytecode::disassemble(function).unwrap())
        .collect::<Vec<_>>();
    assert!(
        disassembly
            .iter()
            .any(|code| code.contains("LoadEnvironment") && code.contains("depth=1"))
    );
}
