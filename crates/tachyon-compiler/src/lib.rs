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
    HirBinding, HirCatchClause, HirClass, HirClassElement, HirClassField, HirClassMethod,
    HirClassMethodKind, HirExpression, HirExpressionKind, HirForInitializer, HirFunction,
    HirFunctionDeclaration, HirFunctionKind, HirIdentifierReference, HirLogicalOperator,
    HirObjectProperty, HirObjectPropertyKey, HirObjectPropertyValue, HirPattern, HirPatternKind,
    HirPatternProperty, HirPrivateField, HirPrivateName, HirPrivateNameId, HirProgram, HirScope,
    HirScopeFlags, HirStatement, HirStatementKind, HirSwitchCase, HirUnaryOperator,
    HirUpdateOperator, HirVariableDeclaration, HirVariableDeclarationKind, HirVariableDeclarator,
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
mod tests;
