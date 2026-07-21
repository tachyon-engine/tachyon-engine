//! Owned high-level IR that intentionally does not retain Oxc AST nodes or arena lifetimes.

mod expression;
mod pattern;
mod program;
mod statement;

pub use expression::{
    HirAssignmentOperator, HirAssignmentTarget, HirBinaryOperator, HirClass, HirClassElement,
    HirClassField, HirClassMethod, HirClassMethodKind, HirExpression, HirExpressionKind,
    HirLogicalOperator, HirObjectExpressionPart, HirObjectProperty, HirObjectPropertyKey,
    HirObjectPropertyValue, HirPrivateAccessor, HirPrivateField, HirPrivateMethod, HirPrivateName,
    HirPrivateNameId, HirUnaryOperator, HirUpdateOperator,
};
pub use pattern::{HirPattern, HirPatternKind, HirPatternProperty};
pub(crate) use program::lower;
pub use program::{
    BindingId, FunctionStencilId, HirBinding, HirFunction, HirFunctionDeclaration, HirFunctionKind,
    HirIdentifierReference, HirProgram, HirScope, HirScopeFlags, ReferenceId, ScopeId,
    StatementCompletion,
};
pub use statement::{
    HirCatchClause, HirForInLeft, HirForInitializer, HirStatement, HirStatementKind, HirSwitchCase,
    HirVariableDeclaration, HirVariableDeclarationKind, HirVariableDeclarator,
};

use crate::{CompileError, SourceName, SourceSpan, SourceText};

fn to_scope_id(id: oxc::semantic::ScopeId) -> ScopeId {
    ScopeId(id.index() as u32)
}

fn missing_semantic(source: &SourceText, span: SourceSpan, semantic: &'static str) -> CompileError {
    CompileError::MissingSemanticId {
        source_name: source.name().clone(),
        span,
        semantic,
    }
}

fn unsupported(source_name: &SourceName, span: SourceSpan, syntax: &'static str) -> CompileError {
    CompileError::UnsupportedSyntax {
        source_name: source_name.clone(),
        span,
        syntax,
    }
}

fn source_span(span: oxc::span::Span) -> SourceSpan {
    SourceSpan {
        start: span.start,
        end: span.end,
    }
}
