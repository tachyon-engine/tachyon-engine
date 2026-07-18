#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Oxc-facing compilation from caller-provided source text to owned bytecode.
//!
//! Source loading remains a host responsibility; this crate intentionally has no host I/O surface.

mod bytecode;
mod diagnostic;
mod hir;
mod parser;
mod source;

use std::sync::Arc;

pub use diagnostic::{Diagnostic, DiagnosticSeverity, RelatedDiagnosticSpan, SourceSpan};
pub use hir::{
    BindingId, FunctionStencilId, HirAssignmentOperator, HirAssignmentTarget, HirBinaryOperator,
    HirBinding, HirCatchClause, HirExpression, HirExpressionKind, HirForInitializer, HirFunction,
    HirFunctionDeclaration, HirIdentifierReference, HirLogicalOperator, HirObjectProperty,
    HirObjectPropertyKey, HirProgram, HirScope, HirScopeFlags, HirStatement, HirStatementKind,
    HirSwitchCase, HirUnaryOperator, HirUpdateOperator, HirVariableDeclaration,
    HirVariableDeclarationKind, HirVariableDeclarator, ReferenceId, ScopeId, StatementCompletion,
};
pub use parser::{ParsedSource, ProgramKind};
pub use source::{CompileOptions, MediaType, SourceId, SourceMode, SourceName, SourceText};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    SourceTooLarge {
        source_name: SourceName,
        byte_len: usize,
    },
    Diagnostics(Arc<[Diagnostic]>),
    UnsupportedSyntax {
        source_name: SourceName,
        span: SourceSpan,
        syntax: &'static str,
    },
    Builder(tachyon_bytecode::BuilderError),
    Module(tachyon_bytecode::ModuleBuildError),
    ConstantOverflow,
    ConstantAllocationFailed,
    RegisterOverflow,
    BindingOverflow,
    MissingSemanticId {
        source_name: SourceName,
        span: SourceSpan,
        semantic: &'static str,
    },
    LoweringCapacityOverflow {
        collection: &'static str,
    },
    UnboundExceptionHandler,
}

/// The stateless frontend boundary; source and all host configuration must be supplied per call.
#[derive(Clone, Copy, Debug, Default)]
pub struct Compiler;

impl Compiler {
    /// Parses source into owned frontend data and guarantees no Oxc arena or semantic value escapes this call.
    pub fn parse(
        &self,
        source: SourceText,
        options: CompileOptions,
    ) -> Result<ParsedSource, CompileError> {
        parser::parse(source, options)
    }

    /// Builds an owned HIR while Oxc's arena is alive, then drops the AST and allocator before returning.
    pub fn lower_to_hir(
        &self,
        source: SourceText,
        options: CompileOptions,
    ) -> Result<HirProgram, CompileError> {
        let (_, hir) = parser::parse_with(source, options, hir::lower)?;
        Ok(hir)
    }

    /// Compiles the supported HIR subset into a verified immutable module without accessing host I/O.
    pub fn compile(
        &self,
        source: SourceText,
        options: CompileOptions,
    ) -> Result<tachyon_bytecode::CompiledModule, CompileError> {
        let (parsed, hir) = parser::parse_with(source, options, hir::lower)?;
        bytecode::lower(parsed.source(), &hir)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proptest::prelude::*;

    use super::*;

    fn source(media_type: MediaType, text: &str) -> SourceText {
        SourceText::new(
            SourceId::new(7),
            SourceName::new("embedded-input"),
            media_type,
            Arc::from(text),
        )
    }

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
        assert_eq!(declarator.binding.name.as_ref(), "answer");
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
        let HirStatementKind::VariableDeclaration(outer_declaration) = &outer_declaration.kind
        else {
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
        let outer = &outer_declaration.declarators[0].binding;
        let inner = &inner_declaration.declarators[0].binding;
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

    #[test]
    fn compiler_emits_verified_bytecode_for_one_plus_two() {
        let module = Compiler
            .compile(
                source(MediaType::JavaScript, "1 + 2;"),
                CompileOptions::default(),
            )
            .unwrap();
        let disassembly = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(0))
                .unwrap(),
        )
        .unwrap();
        assert!(disassembly.contains("LoadImmediate r0, imm=1"));
        assert!(disassembly.contains("LoadImmediate r1, imm=2"));
        assert!(disassembly.contains("Add r2, r0, r1"));
        assert!(disassembly.contains("Return r2"));
    }

    #[test]
    fn function_hir_owns_parameters_body_and_direct_call() {
        let hir = Compiler
            .lower_to_hir(
                source(
                    MediaType::JavaScript,
                    "function addTwo(value) { return value + 2; } addTwo(40);",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let [function] = hir.functions() else {
            panic!("expected one owned function stencil");
        };
        assert_eq!(function.name.as_deref(), Some("addTwo"));
        assert_eq!(function.parameters[0].name.as_ref(), "value");
        assert!(matches!(function.body[0].kind, HirStatementKind::Return(_)));
        assert!(matches!(
            hir.statements().last().map(|statement| &statement.kind),
            Some(HirStatementKind::Expression(HirExpression {
                kind: HirExpressionKind::Call { .. },
                ..
            }))
        ));
    }

    #[test]
    /// Preserves parameter initializer expressions in owned HIR after the Oxc arena is discarded.
    fn function_hir_owns_default_parameter_initializers() {
        let hir = Compiler
            .lower_to_hir(
                source(
                    MediaType::JavaScript,
                    "function add(value = 40, next = value + 1) { return next; }",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let [function] = hir.functions() else {
            panic!("expected one function stencil");
        };
        assert_eq!(function.parameter_initializers.len(), 2);
        assert!(function.parameter_initializers[0].is_some());
        assert!(matches!(
            function.parameter_initializers[1],
            Some(HirExpression {
                kind: HirExpressionKind::Binary { .. },
                ..
            })
        ));
    }

    #[test]
    fn compiler_emits_hoisted_closure_call_and_ordinary_function() {
        let module = Compiler
            .compile(
                source(
                    MediaType::JavaScript,
                    "addTwo(40); function addTwo(value) { return value + 2; }",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        assert_eq!(module.functions().len(), 2);
        let entry = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(0))
                .unwrap(),
        )
        .unwrap();
        let function = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(1))
                .unwrap(),
        )
        .unwrap();
        assert!(entry.contains("CreateClosure r0, function=1"));
        assert!(entry.contains("StoreScope r0, scope=0"));
        assert_eq!(module.scope_names(), &[Arc::from("addTwo")]);
        assert!(entry.contains("Call "));
        assert!(entry.contains("argc=1"));
        assert!(function.contains("Add r2, r0, r1"));
        assert!(function.contains("Return r2"));
    }

    #[test]
    /// Empty and bare-return functions require no register solely to materialize undefined.
    fn compiler_uses_zero_register_return_undefined_for_known_completions() {
        let module = Compiler
            .compile(
                source(
                    MediaType::JavaScript,
                    "function implicit() {} function explicit() { return; } implicit(); explicit();",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        for function_id in [1, 2] {
            let function = module
                .function(tachyon_bytecode::FunctionId::new(function_id))
                .unwrap();
            let disassembly = tachyon_bytecode::disassemble(function).unwrap();
            assert_eq!(function.layout().register_count, 0);
            assert!(disassembly.contains("ReturnUndefined"));
            assert!(!disassembly.contains("LoadUndefined"));
        }
    }

    #[test]
    /// Confirms script vars become deduplicated global declarations while function vars use frames.
    fn compiler_instantiates_var_bindings_at_their_scope_entry() {
        let module = Compiler
            .compile(
                source(
                    MediaType::JavaScript,
                    "var outer; { var inner = 2; } function read(value) { return typeof local; var local = value; } inner;",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let entry = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(0))
                .unwrap(),
        )
        .unwrap();
        let function = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(1))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(entry.matches("DeclareScope").count(), 2);
        assert!(entry.contains("StoreScope"));
        assert!(function.contains("LoadUndefined"));
        assert!(function.contains("Typeof"));
    }

    #[test]
    /// A fully terminal try/catch must not leave a jump whose target is the function code end.
    fn terminal_try_catch_freezes_only_instruction_boundary_targets() {
        let module = Compiler
            .compile(
                source(
                    MediaType::JavaScript,
                    "function render(value) { var basic = value; if (basic) return basic; try { return value; } catch (error) { if (error) { return 1; } throw error; } } render(2);",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let function = module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap();
        assert!(tachyon_bytecode::disassemble(function).is_ok());
    }

    #[test]
    fn compiler_lowers_instanceof_to_its_verified_opcode() {
        let module = Compiler
            .compile(
                source(
                    MediaType::JavaScript,
                    "function Constructor() {} new Constructor() instanceof Constructor;",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let entry = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(0))
                .unwrap(),
        )
        .unwrap();
        assert!(entry.contains("InstanceOf"));
    }

    #[test]
    fn compiler_lowers_nonlocal_identifier_writes_to_resolved_scope_stores() {
        let module = Compiler
            .compile(
                source(
                    MediaType::JavaScript,
                    "var value = 1; function update() { value += 2; value++; } update(); value = 7;",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let entry = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(0))
                .unwrap(),
        )
        .unwrap();
        let function = tachyon_bytecode::disassemble(
            module
                .function(tachyon_bytecode::FunctionId::new(1))
                .unwrap(),
        )
        .unwrap();
        assert!(entry.contains("StoreResolvedScope"));
        assert_eq!(function.matches("StoreResolvedScope").count(), 2);
    }

    #[test]
    /// Proves compiler-selected frame/global locations are frozen alongside verified functions.
    fn compiler_emits_binding_plans_for_local_and_global_storage() {
        let module = Compiler
            .compile(
                source(
                    MediaType::JavaScript,
                    "var global = 1; function read(param) { let local = param; const fixed = local; return fixed + global; }",
                ),
                CompileOptions::default(),
            )
            .unwrap();
        let entry = module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap();
        let function = module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap();

        assert!(entry.binding_plan().iter().any(|binding| {
            binding.location == tachyon_bytecode::BindingLocation::GlobalProperty
                && binding.name.as_ref() == "global"
        }));
        assert_eq!(
            function
                .binding_plan()
                .iter()
                .filter(|binding| matches!(
                    binding.location,
                    tachyon_bytecode::BindingLocation::FrameRegister(_)
                ))
                .count(),
            3
        );
        assert!(
            function
                .binding_plan()
                .iter()
                .any(|binding| { !binding.mutable && binding.name.as_ref() == "fixed" })
        );
        assert!(function.binding_plan().iter().any(|binding| {
            binding.location == tachyon_bytecode::BindingLocation::GlobalProperty
                && binding.name.as_ref() == "global"
        }));
    }

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
    fn compiler_keeps_finally_explicitly_unsupported_until_completion_replay() {
        let error = Compiler
            .compile(
                source(MediaType::JavaScript, "try { 1; } finally { 2; }"),
                CompileOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CompileError::UnsupportedSyntax {
                syntax: "finally statement",
                ..
            }
        ));
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
}
