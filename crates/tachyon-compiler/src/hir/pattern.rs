//! Owned binding and assignment patterns copied from Oxc before its arena is released.

use std::sync::Arc;

use oxc::{
    ast::ast::{
        ArrayAssignmentTarget, AssignmentTarget, AssignmentTargetMaybeDefault,
        AssignmentTargetProperty, BindingPattern, ObjectAssignmentTarget, PropertyKey,
    },
    semantic::{ScopeFlags as OxcScopeFlags, Semantic},
    span::GetSpan,
};

use crate::{CompileError, SourceSpan, SourceText};

use super::{
    expression::{
        HirAssignmentTarget, HirExpression, HirObjectPropertyKey, lower_assignment_target,
        lower_expression,
    },
    program::{HirBinding, HirFunction},
    source_span, to_scope_id, unsupported,
};

#[derive(Clone, Debug, PartialEq)]
pub struct HirPattern {
    pub span: SourceSpan,
    pub kind: HirPatternKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirPatternKind {
    Binding(HirBinding),
    Assignment(HirAssignmentTarget),
    Default {
        target: Box<HirPattern>,
        initializer: Box<HirExpression>,
    },
    Array {
        elements: Arc<[Option<HirPattern>]>,
        rest: Option<Box<HirPattern>>,
    },
    Object {
        properties: Arc<[HirPatternProperty]>,
        rest: Option<Box<HirPattern>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirPatternProperty {
    pub span: SourceSpan,
    pub key: HirObjectPropertyKey,
    pub target: HirPattern,
}

impl HirPattern {
    /// Returns the identifier name eligible for SetFunctionName in a default initializer.
    pub(crate) fn inferred_name(&self) -> Option<&Arc<str>> {
        match &self.kind {
            HirPatternKind::Binding(binding) => Some(&binding.name),
            HirPatternKind::Assignment(super::expression::HirAssignmentTarget::Identifier(
                reference,
            )) => Some(&reference.name),
            _ => None,
        }
    }

    /// Returns the declaration leaf used by the bytecode subset until destructuring is implemented.
    #[inline(always)]
    pub(crate) fn binding(&self) -> Option<&HirBinding> {
        match &self.kind {
            HirPatternKind::Binding(binding) => Some(binding),
            _ => None,
        }
    }

    /// Returns the assignment leaf used by the bytecode subset until destructuring is implemented.
    #[inline(always)]
    pub(crate) fn assignment_target(&self) -> Option<&HirAssignmentTarget> {
        match &self.kind {
            HirPatternKind::Assignment(target) => Some(target),
            _ => None,
        }
    }
}

/// Copies one binding pattern and every nested default/key expression into owned HIR.
pub(super) fn lower_binding_pattern(
    pattern: &BindingPattern<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirPattern, CompileError> {
    let span = source_span(pattern.span());
    let kind = match pattern {
        BindingPattern::BindingIdentifier(identifier) => {
            HirPatternKind::Binding(new_binding(identifier, source, semantic)?)
        }
        BindingPattern::AssignmentPattern(pattern) => HirPatternKind::Default {
            target: Box::new(lower_binding_pattern(
                &pattern.left,
                source,
                semantic,
                functions,
            )?),
            initializer: Box::new(lower_expression(
                &pattern.right,
                source,
                semantic,
                functions,
            )?),
        },
        BindingPattern::ArrayPattern(pattern) => {
            let mut elements = Vec::with_capacity(pattern.elements.len());
            for element in &pattern.elements {
                elements.push(
                    element
                        .as_ref()
                        .map(|element| lower_binding_pattern(element, source, semantic, functions))
                        .transpose()?,
                );
            }
            let rest = pattern
                .rest
                .as_ref()
                .map(|rest| {
                    lower_binding_pattern(&rest.argument, source, semantic, functions).map(Box::new)
                })
                .transpose()?;
            HirPatternKind::Array {
                elements: elements.into(),
                rest,
            }
        }
        BindingPattern::ObjectPattern(pattern) => {
            let mut properties = Vec::with_capacity(pattern.properties.len());
            for property in &pattern.properties {
                properties.push(HirPatternProperty {
                    span: source_span(property.span),
                    key: lower_pattern_key(
                        &property.key,
                        property.computed,
                        source,
                        semantic,
                        functions,
                    )?,
                    target: lower_binding_pattern(&property.value, source, semantic, functions)?,
                });
            }
            let rest = pattern
                .rest
                .as_ref()
                .map(|rest| {
                    lower_binding_pattern(&rest.argument, source, semantic, functions).map(Box::new)
                })
                .transpose()?;
            HirPatternKind::Object {
                properties: properties.into(),
                rest,
            }
        }
    };
    Ok(HirPattern { span, kind })
}

/// Copies a simple or destructuring assignment target without conflating it with declarations.
pub(super) fn lower_assignment_pattern(
    target: &AssignmentTarget<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirPattern, CompileError> {
    let span = source_span(target.span());
    let kind = match target {
        AssignmentTarget::ArrayAssignmentTarget(pattern) => {
            lower_array_assignment_pattern(pattern, source, semantic, functions)?
        }
        AssignmentTarget::ObjectAssignmentTarget(pattern) => {
            lower_object_assignment_pattern(pattern, source, semantic, functions)?
        }
        _ => HirPatternKind::Assignment(lower_assignment_target(
            target, source, semantic, functions,
        )?),
    };
    Ok(HirPattern { span, kind })
}

/// Copies array assignment elements, preserving elisions, defaults, and the exact rest target.
fn lower_array_assignment_pattern(
    pattern: &ArrayAssignmentTarget<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirPatternKind, CompileError> {
    let mut elements = Vec::with_capacity(pattern.elements.len());
    for element in &pattern.elements {
        elements.push(
            element
                .as_ref()
                .map(|element| lower_assignment_maybe_default(element, source, semantic, functions))
                .transpose()?,
        );
    }
    let rest = pattern
        .rest
        .as_ref()
        .map(|rest| {
            lower_assignment_pattern(&rest.target, source, semantic, functions).map(Box::new)
        })
        .transpose()?;
    Ok(HirPatternKind::Array {
        elements: elements.into(),
        rest,
    })
}

/// Copies object assignment properties in source order so computed keys remain observable once.
fn lower_object_assignment_pattern(
    pattern: &ObjectAssignmentTarget<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirPatternKind, CompileError> {
    let mut properties = Vec::with_capacity(pattern.properties.len());
    for property in &pattern.properties {
        let (span, key, target) = match property {
            AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(property) => {
                let target = HirPattern {
                    span: source_span(property.binding.span),
                    kind: HirPatternKind::Assignment(HirAssignmentTarget::Identifier(
                        super::expression::new_reference(&property.binding, source, semantic)?,
                    )),
                };
                let target = if let Some(initializer) = &property.init {
                    HirPattern {
                        span: source_span(property.span),
                        kind: HirPatternKind::Default {
                            target: Box::new(target),
                            initializer: Box::new(lower_expression(
                                initializer,
                                source,
                                semantic,
                                functions,
                            )?),
                        },
                    }
                } else {
                    target
                };
                (
                    source_span(property.span),
                    HirObjectPropertyKey::Static(Arc::from(property.binding.name.as_str())),
                    target,
                )
            }
            AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => (
                source_span(property.span),
                lower_pattern_key(
                    &property.name,
                    property.computed,
                    source,
                    semantic,
                    functions,
                )?,
                lower_assignment_maybe_default(&property.binding, source, semantic, functions)?,
            ),
        };
        properties.push(HirPatternProperty { span, key, target });
    }
    let rest = pattern
        .rest
        .as_ref()
        .map(|rest| {
            lower_assignment_pattern(&rest.target, source, semantic, functions).map(Box::new)
        })
        .transpose()?;
    Ok(HirPatternKind::Object {
        properties: properties.into(),
        rest,
    })
}

fn lower_assignment_maybe_default(
    target: &AssignmentTargetMaybeDefault<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirPattern, CompileError> {
    match target {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(target) => Ok(HirPattern {
            span: source_span(target.span),
            kind: HirPatternKind::Default {
                target: Box::new(lower_assignment_pattern(
                    &target.binding,
                    source,
                    semantic,
                    functions,
                )?),
                initializer: Box::new(lower_expression(&target.init, source, semantic, functions)?),
            },
        }),
        _ => lower_assignment_pattern(target.to_assignment_target(), source, semantic, functions),
    }
}

/// Owns a static or computed property key using the same canonical numeric spelling as literals.
fn lower_pattern_key(
    key: &PropertyKey<'_>,
    computed: bool,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirObjectPropertyKey, CompileError> {
    if computed {
        return lower_expression(key.to_expression(), source, semantic, functions)
            .map(HirObjectPropertyKey::Computed);
    }
    let value = match key {
        PropertyKey::StaticIdentifier(identifier) => Arc::from(identifier.name.as_str()),
        PropertyKey::StringLiteral(literal) => {
            if literal.lone_surrogates {
                return Ok(HirObjectPropertyKey::Computed(HirExpression {
                    span: source_span(literal.span),
                    kind: super::expression::HirExpressionKind::String(super::copy_string_literal(
                        literal, source,
                    )?),
                }));
            }
            Arc::from(literal.value.as_str())
        }
        PropertyKey::NumericLiteral(literal) => {
            let mut buffer = ryu_js::Buffer::new();
            Arc::from(if literal.value == 0.0 {
                "0"
            } else {
                buffer.format(literal.value)
            })
        }
        _ => {
            return Err(unsupported(
                source.name(),
                source_span(key.span()),
                "pattern property key",
            ));
        }
    };
    Ok(HirObjectPropertyKey::Static(value))
}

pub(super) fn new_binding(
    identifier: &oxc::ast::ast::BindingIdentifier<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
) -> Result<HirBinding, CompileError> {
    let span = source_span(identifier.span);
    let symbol = identifier
        .symbol_id
        .get()
        .ok_or_else(|| super::missing_semantic(source, span, "binding symbol"))?;
    let binding_scope = semantic.scoping().symbol_scope_id(symbol);
    let binding_function = nearest_function_scope(semantic, binding_scope);
    let captured = binding_function.is_some_and(|binding_function| {
        semantic
            .scoping()
            .get_resolved_references(symbol)
            .any(|reference| {
                nearest_function_scope(semantic, reference.scope_id()) != Some(binding_function)
            })
    });
    Ok(HirBinding {
        id: super::program::BindingId(symbol.index() as u32),
        scope: to_scope_id(binding_scope),
        span,
        name: Arc::from(identifier.name.as_str()),
        captured,
    })
}

fn nearest_function_scope(
    semantic: &Semantic<'_>,
    mut scope: oxc::semantic::ScopeId,
) -> Option<oxc::semantic::ScopeId> {
    loop {
        if semantic
            .scoping()
            .scope_flags(scope)
            .intersects(OxcScopeFlags::Function | OxcScopeFlags::ClassStaticBlock)
        {
            return Some(scope);
        }
        scope = semantic.scoping().scope_parent_id(scope)?;
    }
}
