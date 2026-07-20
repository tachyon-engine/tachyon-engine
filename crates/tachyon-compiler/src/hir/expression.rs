use std::sync::Arc;

use oxc::{
    ast::ast::{
        ArrayExpressionElement, AssignmentTarget, Expression, ObjectPropertyKind, PropertyKey,
        PropertyKind, SimpleAssignmentTarget,
    },
    semantic::Semantic,
    span::{GetSpan, Span},
    syntax::operator::{
        AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
    },
};

use crate::{CompileError, SourceSpan, SourceText};

use super::pattern::{HirPattern, lower_assignment_pattern, new_binding};
use super::program::{
    BindingId, FunctionStencilId, HirFunction, HirIdentifierReference, ReferenceId,
};
use super::statement::{lower_arrow_function_stencil, lower_function_stencil};
use super::{missing_semantic, source_span, to_scope_id, unsupported};

#[derive(Clone, Debug, PartialEq)]
pub struct HirObjectProperty {
    pub span: SourceSpan,
    pub key: HirObjectPropertyKey,
    pub value: HirObjectPropertyValue,
}

/// One ordered object-literal item retained when a literal contains a spread expression.
#[derive(Clone, Debug, PartialEq)]
pub enum HirObjectExpressionPart {
    Property(HirObjectProperty),
    Spread(HirExpression),
}

/// Owns the definition mode for one object or array literal property.
#[derive(Clone, Debug, PartialEq)]
pub enum HirObjectPropertyValue {
    Data(HirExpression),
    Getter(FunctionStencilId),
    Setter(FunctionStencilId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirObjectPropertyKey {
    Static(Arc<str>),
    Computed(HirExpression),
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirExpression {
    pub span: SourceSpan,
    pub kind: HirExpressionKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirAssignmentTarget {
    Identifier(HirIdentifierReference),
    StaticMember {
        object: Box<HirExpression>,
        property: Arc<str>,
    },
    ComputedMember {
        object: Box<HirExpression>,
        property: Box<HirExpression>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirAssignmentOperator {
    Assign,
    Binary(HirBinaryOperator),
    Logical(HirLogicalOperator),
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpressionKind {
    Number(u64),
    String(Arc<str>),
    RegExp {
        pattern: Arc<str>,
        flags: u8,
    },
    Boolean(bool),
    Null,
    Identifier(HirIdentifierReference),
    Function(FunctionStencilId),
    This,
    NewTarget,
    Sequence(Arc<[HirExpression]>),
    Object(Arc<[HirObjectProperty]>),
    ObjectSpread(Arc<[HirObjectExpressionPart]>),
    Array(Arc<[HirObjectProperty]>),
    StaticMember {
        object: Box<HirExpression>,
        property: Arc<str>,
    },
    ComputedMember {
        object: Box<HirExpression>,
        property: Box<HirExpression>,
    },
    Unary {
        operator: HirUnaryOperator,
        argument: Box<HirExpression>,
    },
    Binary {
        operator: HirBinaryOperator,
        left: Box<HirExpression>,
        right: Box<HirExpression>,
    },
    Logical {
        operator: HirLogicalOperator,
        left: Box<HirExpression>,
        right: Box<HirExpression>,
    },
    Assignment {
        operator: HirAssignmentOperator,
        target: Box<HirPattern>,
        value: Box<HirExpression>,
    },
    Update {
        operator: HirUpdateOperator,
        prefix: bool,
        target: HirAssignmentTarget,
    },
    Conditional {
        test: Box<HirExpression>,
        consequent: Box<HirExpression>,
        alternate: Box<HirExpression>,
    },
    Call {
        callee: Box<HirExpression>,
        arguments: Arc<[HirExpression]>,
    },
    New {
        callee: Box<HirExpression>,
        arguments: Arc<[HirExpression]>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirUnaryOperator {
    Plus,
    Negate,
    Not,
    BitwiseNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirUpdateOperator {
    Increment,
    Decrement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirBinaryOperator {
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    LessThan,
    LessEqual,
    GreaterThan,
    GreaterEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Exponentiate,
    ShiftLeft,
    ShiftRight,
    ShiftRightUnsigned,
    BitwiseOr,
    BitwiseXor,
    BitwiseAnd,
    In,
    InstanceOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirLogicalOperator {
    And,
    Or,
    Coalesce,
}

/// Recursively copies leaf values and operands so the returned expression has no arena-backed memory.
pub(super) fn lower_expression(
    expression: &Expression<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirExpression, CompileError> {
    let span = source_span(expression.span());
    let kind = match expression {
        Expression::NumericLiteral(literal) => HirExpressionKind::Number(literal.value.to_bits()),
        Expression::StringLiteral(literal) => {
            HirExpressionKind::String(Arc::from(literal.value.as_str()))
        }
        Expression::RegExpLiteral(literal) => HirExpressionKind::RegExp {
            pattern: Arc::from(literal.regex.pattern.text.as_str()),
            flags: literal.regex.flags.bits(),
        },
        Expression::BooleanLiteral(literal) => HirExpressionKind::Boolean(literal.value),
        Expression::NullLiteral(_) => HirExpressionKind::Null,
        Expression::Identifier(identifier) => {
            HirExpressionKind::Identifier(new_reference(identifier, source, semantic)?)
        }
        Expression::FunctionExpression(function) => {
            let self_binding = function
                .id
                .as_ref()
                .map(|identifier| new_binding(identifier, source, semantic))
                .transpose()?;
            let name = self_binding.as_ref().map(|binding| binding.name.clone());
            HirExpressionKind::Function(lower_function_stencil(
                function,
                name,
                self_binding,
                source,
                semantic,
                functions,
            )?)
        }
        Expression::ArrowFunctionExpression(function) => HirExpressionKind::Function(
            lower_arrow_function_stencil(function, source, semantic, functions)?,
        ),
        Expression::ThisExpression(_) => HirExpressionKind::This,
        Expression::MetaProperty(property)
            if property.meta.name == "new" && property.property.name == "target" =>
        {
            HirExpressionKind::NewTarget
        }
        Expression::SequenceExpression(sequence) => {
            let mut expressions = Vec::with_capacity(sequence.expressions.len());
            for expression in &sequence.expressions {
                expressions.push(lower_expression(expression, source, semantic, functions)?);
            }
            HirExpressionKind::Sequence(expressions.into())
        }
        Expression::ArrayExpression(array) => {
            let mut accumulated = None;
            let mut chunk_properties = Vec::new();
            let mut chunk_length = 0usize;
            for element in &array.elements {
                if let Some(value) = element.as_expression() {
                    chunk_properties.push(HirObjectProperty {
                        span: source_span(element.span()),
                        key: HirObjectPropertyKey::Static(Arc::from(chunk_length.to_string())),
                        value: HirObjectPropertyValue::Data(lower_expression(
                            value, source, semantic, functions,
                        )?),
                    });
                    chunk_length += 1;
                    continue;
                }
                if element.is_spread() {
                    let spread = match element {
                        ArrayExpressionElement::SpreadElement(spread) => &spread.argument,
                        _ => {
                            return Err(unsupported(
                                source.name(),
                                source_span(element.span()),
                                "array spread",
                            ));
                        }
                    };
                    let spread = lower_expression(spread, source, semantic, functions)?;
                    let chunk = lower_array_chunk(chunk_properties, chunk_length, span);
                    accumulated = Some(match accumulated {
                        Some(left) => lower_array_concat(left, spread, span),
                        None => lower_array_concat(chunk, spread, span),
                    });
                    chunk_properties = Vec::new();
                    chunk_length = 0;
                } else {
                    chunk_length += 1;
                }
            }
            let tail = lower_array_chunk(chunk_properties, chunk_length, span);
            match accumulated {
                Some(left) if chunk_length != 0 => lower_array_concat(left, tail, span).kind,
                Some(left) => left.kind,
                None => tail.kind,
            }
        }
        Expression::ObjectExpression(expression) => {
            let mut properties = Vec::with_capacity(expression.properties.len());
            let mut saw_spread = false;
            for property in &expression.properties {
                let ObjectPropertyKind::ObjectProperty(property) = property else {
                    let ObjectPropertyKind::SpreadProperty(spread) = property else {
                        unreachable!("object property pattern is exhaustive");
                    };
                    properties.push(HirObjectExpressionPart::Spread(lower_expression(
                        &spread.argument,
                        source,
                        semantic,
                        functions,
                    )?));
                    saw_spread = true;
                    continue;
                };
                let key = if property.computed {
                    HirObjectPropertyKey::Computed(lower_expression(
                        property.key.to_expression(),
                        source,
                        semantic,
                        functions,
                    )?)
                } else {
                    let key: Arc<str> = match &property.key {
                        PropertyKey::StaticIdentifier(identifier) => {
                            Arc::from(identifier.name.as_str())
                        }
                        PropertyKey::StringLiteral(literal) => Arc::from(literal.value.as_str()),
                        PropertyKey::NumericLiteral(literal) => {
                            let mut buffer = ryu_js::Buffer::new();
                            let key = if literal.value == 0.0 {
                                "0"
                            } else {
                                buffer.format(literal.value)
                            };
                            Arc::from(key)
                        }
                        _ => {
                            return Err(unsupported(
                                source.name(),
                                source_span(property.key.span()),
                                "object property key",
                            ));
                        }
                    };
                    HirObjectPropertyKey::Static(key)
                };
                let value = if property.kind == PropertyKind::Get
                    || property.kind == PropertyKind::Set
                {
                    let Expression::FunctionExpression(function) = &property.value else {
                        return Err(unsupported(
                            source.name(),
                            source_span(property.span),
                            "object accessor value",
                        ));
                    };
                    let function =
                        lower_function_stencil(function, None, None, source, semantic, functions)?;
                    if property.kind == PropertyKind::Get {
                        HirObjectPropertyValue::Getter(function)
                    } else {
                        HirObjectPropertyValue::Setter(function)
                    }
                } else if property.method {
                    let Expression::FunctionExpression(function) = &property.value else {
                        return Err(unsupported(
                            source.name(),
                            source_span(property.span),
                            "object method value",
                        ));
                    };
                    if function.id.is_some() {
                        return Err(unsupported(
                            source.name(),
                            source_span(function.span),
                            "named object method",
                        ));
                    }
                    HirObjectPropertyValue::Data(HirExpression {
                        span: source_span(function.span),
                        kind: HirExpressionKind::Function(lower_function_stencil(
                            function, None, None, source, semantic, functions,
                        )?),
                    })
                } else {
                    HirObjectPropertyValue::Data(lower_expression(
                        &property.value,
                        source,
                        semantic,
                        functions,
                    )?)
                };
                properties.push(HirObjectExpressionPart::Property(HirObjectProperty {
                    span: source_span(property.span),
                    key,
                    value,
                }));
            }
            if !saw_spread {
                let properties = properties
                    .into_iter()
                    .map(|part| match part {
                        HirObjectExpressionPart::Property(property) => property,
                        HirObjectExpressionPart::Spread(_) => {
                            unreachable!("spread flag tracks parts")
                        }
                    })
                    .collect::<Vec<_>>();
                HirExpressionKind::Object(properties.into())
            } else {
                HirExpressionKind::ObjectSpread(properties.into())
            }
        }
        Expression::StaticMemberExpression(expression) if !expression.optional => {
            HirExpressionKind::StaticMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                property: Arc::from(expression.property.name.as_str()),
            }
        }
        Expression::ComputedMemberExpression(expression) if !expression.optional => {
            HirExpressionKind::ComputedMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                property: Box::new(lower_expression(
                    &expression.expression,
                    source,
                    semantic,
                    functions,
                )?),
            }
        }
        Expression::UnaryExpression(expression) => HirExpressionKind::Unary {
            operator: lower_unary_operator(expression.operator),
            argument: Box::new(lower_expression(
                &expression.argument,
                source,
                semantic,
                functions,
            )?),
        },
        Expression::BinaryExpression(expression) => HirExpressionKind::Binary {
            operator: lower_binary_operator(expression.operator),
            left: Box::new(lower_expression(
                &expression.left,
                source,
                semantic,
                functions,
            )?),
            right: Box::new(lower_expression(
                &expression.right,
                source,
                semantic,
                functions,
            )?),
        },
        Expression::LogicalExpression(expression) => HirExpressionKind::Logical {
            operator: lower_logical_operator(expression.operator),
            left: Box::new(lower_expression(
                &expression.left,
                source,
                semantic,
                functions,
            )?),
            right: Box::new(lower_expression(
                &expression.right,
                source,
                semantic,
                functions,
            )?),
        },
        Expression::AssignmentExpression(expression) => HirExpressionKind::Assignment {
            operator: lower_assignment_operator(expression.operator, source, expression.span)?,
            target: Box::new(lower_assignment_pattern(
                &expression.left,
                source,
                semantic,
                functions,
            )?),
            value: Box::new(lower_expression(
                &expression.right,
                source,
                semantic,
                functions,
            )?),
        },
        Expression::UpdateExpression(expression) => HirExpressionKind::Update {
            operator: match expression.operator {
                UpdateOperator::Increment => HirUpdateOperator::Increment,
                UpdateOperator::Decrement => HirUpdateOperator::Decrement,
            },
            prefix: expression.prefix,
            target: lower_update_target(&expression.argument, source, semantic, functions)?,
        },
        Expression::ConditionalExpression(expression) => HirExpressionKind::Conditional {
            test: Box::new(lower_expression(
                &expression.test,
                source,
                semantic,
                functions,
            )?),
            consequent: Box::new(lower_expression(
                &expression.consequent,
                source,
                semantic,
                functions,
            )?),
            alternate: Box::new(lower_expression(
                &expression.alternate,
                source,
                semantic,
                functions,
            )?),
        },
        Expression::CallExpression(expression) if !expression.optional => {
            let mut arguments = Vec::with_capacity(expression.arguments.len());
            for argument in &expression.arguments {
                let argument = argument.as_expression().ok_or_else(|| {
                    unsupported(
                        source.name(),
                        source_span(argument.span()),
                        "spread argument",
                    )
                })?;
                arguments.push(lower_expression(argument, source, semantic, functions)?);
            }
            HirExpressionKind::Call {
                callee: Box::new(lower_expression(
                    &expression.callee,
                    source,
                    semantic,
                    functions,
                )?),
                arguments: arguments.into(),
            }
        }
        Expression::NewExpression(expression) if expression.type_arguments.is_none() => {
            let mut arguments = Vec::with_capacity(expression.arguments.len());
            for argument in &expression.arguments {
                let argument = argument.as_expression().ok_or_else(|| {
                    unsupported(
                        source.name(),
                        source_span(argument.span()),
                        "spread constructor argument",
                    )
                })?;
                arguments.push(lower_expression(argument, source, semantic, functions)?);
            }
            HirExpressionKind::New {
                callee: Box::new(lower_expression(
                    &expression.callee,
                    source,
                    semantic,
                    functions,
                )?),
                arguments: arguments.into(),
            }
        }
        Expression::ParenthesizedExpression(expression) => {
            let mut lowered =
                lower_expression(&expression.expression, source, semantic, functions)?;
            lowered.span = span;
            return Ok(lowered);
        }
        _ => return Err(unsupported(source.name(), span, "expression")),
    };
    Ok(HirExpression { span, kind })
}

/// Builds one owned array chunk used by spread lowering and sparse-element preservation.
fn lower_array_chunk(
    mut properties: Vec<HirObjectProperty>,
    length: usize,
    span: SourceSpan,
) -> HirExpression {
    properties.push(HirObjectProperty {
        span,
        key: HirObjectPropertyKey::Static(Arc::from("length")),
        value: HirObjectPropertyValue::Data(HirExpression {
            span,
            kind: HirExpressionKind::Number((length as f64).to_bits()),
        }),
    });
    HirExpression {
        span,
        kind: HirExpressionKind::Array(properties.into()),
    }
}

/// Reifies one spread chunk as a receiver-preserving Array.prototype.concat call.
fn lower_array_concat(
    left: HirExpression,
    right: HirExpression,
    span: SourceSpan,
) -> HirExpression {
    HirExpression {
        span,
        kind: HirExpressionKind::Call {
            callee: Box::new(HirExpression {
                span,
                kind: HirExpressionKind::StaticMember {
                    object: Box::new(left),
                    property: Arc::from("concat"),
                },
            }),
            arguments: Arc::from([right]),
        },
    }
}

/// Converts assignment operators without hiding unsupported short-circuit assignment semantics.
fn lower_assignment_operator(
    operator: AssignmentOperator,
    source: &SourceText,
    span: Span,
) -> Result<HirAssignmentOperator, CompileError> {
    if operator.is_assign() {
        return Ok(HirAssignmentOperator::Assign);
    }
    if let Some(binary) = operator.to_binary_operator().map(lower_binary_operator) {
        return Ok(HirAssignmentOperator::Binary(binary));
    }
    let logical = match operator {
        AssignmentOperator::LogicalAnd => HirLogicalOperator::And,
        AssignmentOperator::LogicalOr => HirLogicalOperator::Or,
        AssignmentOperator::LogicalNullish => HirLogicalOperator::Coalesce,
        _ => {
            return Err(unsupported(
                source.name(),
                source_span(span),
                "assignment operator",
            ));
        }
    };
    Ok(HirAssignmentOperator::Logical(logical))
}

/// Owns identifier and static-member references while rejecting patterns and computed properties.
pub(super) fn lower_assignment_target(
    target: &AssignmentTarget<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirAssignmentTarget, CompileError> {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(identifier) => Ok(
            HirAssignmentTarget::Identifier(new_reference(identifier, source, semantic)?),
        ),
        AssignmentTarget::StaticMemberExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::StaticMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                property: Arc::from(expression.property.name.as_str()),
            })
        }
        AssignmentTarget::ComputedMemberExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::ComputedMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                property: Box::new(lower_expression(
                    &expression.expression,
                    source,
                    semantic,
                    functions,
                )?),
            })
        }
        _ => Err(unsupported(
            source.name(),
            source_span(target.span()),
            "assignment target",
        )),
    }
}

/// Owns update references separately because Oxc excludes destructuring targets by construction.
fn lower_update_target(
    target: &SimpleAssignmentTarget<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirAssignmentTarget, CompileError> {
    match target {
        SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => Ok(
            HirAssignmentTarget::Identifier(new_reference(identifier, source, semantic)?),
        ),
        SimpleAssignmentTarget::StaticMemberExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::StaticMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                property: Arc::from(expression.property.name.as_str()),
            })
        }
        SimpleAssignmentTarget::ComputedMemberExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::ComputedMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                property: Box::new(lower_expression(
                    &expression.expression,
                    source,
                    semantic,
                    functions,
                )?),
            })
        }
        _ => Err(unsupported(
            source.name(),
            source_span(target.span()),
            "update target",
        )),
    }
}

/// Copies one Oxc semantic reference without retaining its arena-owned ID or symbol table.
pub(super) fn new_reference(
    identifier: &oxc::ast::ast::IdentifierReference<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
) -> Result<HirIdentifierReference, CompileError> {
    let span = source_span(identifier.span);
    let id = identifier
        .reference_id
        .get()
        .ok_or_else(|| missing_semantic(source, span, "identifier reference"))?;
    let reference = semantic.scoping().get_reference(id);
    let binding_scope = reference
        .symbol_id()
        .map(|symbol| to_scope_id(semantic.scoping().symbol_scope_id(symbol)));
    Ok(HirIdentifierReference {
        id: ReferenceId(id.index() as u32),
        scope: to_scope_id(reference.scope_id()),
        binding: reference
            .symbol_id()
            .map(|symbol| BindingId(symbol.index() as u32)),
        binding_scope,
        name: Arc::from(identifier.name.as_str()),
        read: reference.is_read(),
        write: reference.is_write(),
    })
}

fn lower_unary_operator(operator: UnaryOperator) -> HirUnaryOperator {
    match operator {
        UnaryOperator::UnaryPlus => HirUnaryOperator::Plus,
        UnaryOperator::UnaryNegation => HirUnaryOperator::Negate,
        UnaryOperator::LogicalNot => HirUnaryOperator::Not,
        UnaryOperator::BitwiseNot => HirUnaryOperator::BitwiseNot,
        UnaryOperator::Typeof => HirUnaryOperator::Typeof,
        UnaryOperator::Void => HirUnaryOperator::Void,
        UnaryOperator::Delete => HirUnaryOperator::Delete,
    }
}

fn lower_binary_operator(operator: BinaryOperator) -> HirBinaryOperator {
    match operator {
        BinaryOperator::Equality => HirBinaryOperator::Equal,
        BinaryOperator::Inequality => HirBinaryOperator::NotEqual,
        BinaryOperator::StrictEquality => HirBinaryOperator::StrictEqual,
        BinaryOperator::StrictInequality => HirBinaryOperator::StrictNotEqual,
        BinaryOperator::LessThan => HirBinaryOperator::LessThan,
        BinaryOperator::LessEqualThan => HirBinaryOperator::LessEqual,
        BinaryOperator::GreaterThan => HirBinaryOperator::GreaterThan,
        BinaryOperator::GreaterEqualThan => HirBinaryOperator::GreaterEqual,
        BinaryOperator::Addition => HirBinaryOperator::Add,
        BinaryOperator::Subtraction => HirBinaryOperator::Subtract,
        BinaryOperator::Multiplication => HirBinaryOperator::Multiply,
        BinaryOperator::Division => HirBinaryOperator::Divide,
        BinaryOperator::Remainder => HirBinaryOperator::Remainder,
        BinaryOperator::Exponential => HirBinaryOperator::Exponentiate,
        BinaryOperator::ShiftLeft => HirBinaryOperator::ShiftLeft,
        BinaryOperator::ShiftRight => HirBinaryOperator::ShiftRight,
        BinaryOperator::ShiftRightZeroFill => HirBinaryOperator::ShiftRightUnsigned,
        BinaryOperator::BitwiseOR => HirBinaryOperator::BitwiseOr,
        BinaryOperator::BitwiseXOR => HirBinaryOperator::BitwiseXor,
        BinaryOperator::BitwiseAnd => HirBinaryOperator::BitwiseAnd,
        BinaryOperator::In => HirBinaryOperator::In,
        BinaryOperator::Instanceof => HirBinaryOperator::InstanceOf,
    }
}

fn lower_logical_operator(operator: LogicalOperator) -> HirLogicalOperator {
    match operator {
        LogicalOperator::And => HirLogicalOperator::And,
        LogicalOperator::Or => HirLogicalOperator::Or,
        LogicalOperator::Coalesce => HirLogicalOperator::Coalesce,
    }
}
