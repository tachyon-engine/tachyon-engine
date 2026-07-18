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
    BindingId, FunctionStencilId, HirBinaryOperator, HirBinding, HirExpression, HirExpressionKind,
    HirFunction, HirFunctionDeclaration, HirProgram, HirStatement, HirStatementKind,
    HirUnaryOperator, HirVariableDeclaration, HirVariableDeclarationKind, HirVariableDeclarator,
    ReferenceId, ScopeId, StatementCompletion,
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
    RegisterOverflow,
    BindingOverflow,
    LoweringCapacityOverflow {
        collection: &'static str,
    },
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
        assert_eq!(function.name.as_ref(), "addTwo");
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
        assert!(entry.contains("Call r4, callee=r1, argc=1"));
        assert!(function.contains("Add r2, r0, r1"));
        assert!(function.contains("Return r2"));
    }

    #[test]
    fn compiler_rejects_assignment_to_immutable_local_until_throw_lowering_exists() {
        let error = Compiler
            .compile(
                source(MediaType::JavaScript, "const answer = 42; answer = 0;"),
                CompileOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CompileError::UnsupportedSyntax {
                syntax: "assignment to immutable local",
                ..
            }
        ));
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
