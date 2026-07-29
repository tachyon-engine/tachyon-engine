//! Owned high-level IR that intentionally does not retain Oxc AST nodes or arena lifetimes.

mod expression;
mod module;
mod pattern;
mod program;
mod statement;

pub use expression::{
    HirArrayExpressionPart, HirAssignmentOperator, HirAssignmentTarget, HirBinaryOperator,
    HirClass, HirClassElement, HirClassField, HirClassMethod, HirClassMethodKind, HirExpression,
    HirExpressionKind, HirLogicalOperator, HirObjectExpressionPart, HirObjectProperty,
    HirObjectPropertyKey, HirObjectPropertyValue, HirPrivateAccessor, HirPrivateField,
    HirPrivateMethod, HirPrivateName, HirPrivateNameId, HirUnaryOperator, HirUpdateOperator,
};
pub use module::{
    HirModuleAttribute, HirModuleExportEntry, HirModuleImportEntry, HirModuleImportName,
    HirModuleRequest, HirModuleStencil,
};
pub use pattern::{HirPattern, HirPatternKind, HirPatternProperty};
pub(crate) use program::lexical_arguments_binding;
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

use std::sync::Arc;

use oxc::ast::ast::StringLiteral;

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

/// Copies Oxc's escaped lone-surrogate representation into exact ECMAScript UTF-16 code units.
fn copy_string_literal(
    literal: &StringLiteral<'_>,
    source: &SourceText,
) -> Result<Arc<[u16]>, CompileError> {
    copy_oxc_string_units(
        literal.value.as_str(),
        literal.lone_surrogates,
        source,
        source_span(literal.span),
    )
}

/// Decodes Oxc's `U+FFFDxxxx` sentinel while preserving ordinary Unicode scalar values.
fn copy_oxc_string_units(
    value: &str,
    lone_surrogates: bool,
    source: &SourceText,
    span: SourceSpan,
) -> Result<Arc<[u16]>, CompileError> {
    if !lone_surrogates {
        let mut units = Vec::new();
        units
            .try_reserve_exact(value.encode_utf16().count())
            .map_err(|_| CompileError::ConstantAllocationFailed)?;
        units.extend(value.encode_utf16());
        return Ok(units.into());
    }
    let malformed = || CompileError::MalformedStringLiteral {
        source_name: source.name().clone(),
        span,
    };
    let mut units = Vec::new();
    units
        .try_reserve_exact(value.len())
        .map_err(|_| CompileError::ConstantAllocationFailed)?;
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{fffd}' {
            let mut encoded = [0; 2];
            units.extend_from_slice(ch.encode_utf16(&mut encoded));
            continue;
        }
        let mut unit = 0_u16;
        for _ in 0..4 {
            let digit = chars
                .next()
                .and_then(|digit| digit.to_digit(16))
                .ok_or_else(&malformed)?;
            unit = unit
                .checked_mul(16)
                .and_then(|value| value.checked_add(digit as u16))
                .ok_or_else(&malformed)?;
        }
        units.push(unit);
    }
    Ok(units.into())
}
