use std::sync::Arc;

use oxc::{
    ast::ast::{
        Argument, ArrayExpressionElement, AssignmentTarget, Expression, ObjectPropertyKind,
        PropertyKey, PropertyKind, SimpleAssignmentTarget,
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
use super::statement::{
    StatementContext, lower_arrow_function_stencil, lower_function_stencil, lower_statement,
};
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

/// One ordered ArrayAccumulation item retained when a literal contains spread.
#[derive(Clone, Debug, PartialEq)]
pub enum HirArrayExpressionPart {
    Element(HirExpression),
    Elision,
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
pub struct HirClass {
    pub name: Option<Arc<str>>,
    /// The immutable inner binding created for a class declaration or named expression.
    pub name_binding: Option<super::program::HirBinding>,
    pub scope: super::program::ScopeId,
    pub super_class: Option<Box<HirExpression>>,
    pub constructor: FunctionStencilId,
    pub private_names: Arc<[HirPrivateName]>,
    pub elements: Arc<[HirClassElement]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirClassElement {
    Method(HirClassMethod),
    PublicField(HirClassField),
    PrivateField(HirPrivateField),
    PrivateMethod(HirPrivateMethod),
    PrivateAccessor(HirPrivateAccessor),
    StaticBlock(HirClassStaticBlock),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HirPrivateNameId {
    pub class: u32,
    pub element: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirPrivateName {
    pub id: HirPrivateNameId,
    pub name: Arc<str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirPrivateField {
    pub span: SourceSpan,
    pub name: HirPrivateName,
    pub initializer: Option<FunctionStencilId>,
    pub is_static: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirPrivateMethod {
    pub span: SourceSpan,
    pub name: HirPrivateName,
    pub function: FunctionStencilId,
    pub is_static: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirPrivateAccessor {
    pub span: SourceSpan,
    pub name: HirPrivateName,
    pub getter: Option<FunctionStencilId>,
    pub setter: Option<FunctionStencilId>,
    pub is_static: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HirClassStaticBlock {
    pub span: SourceSpan,
    pub function: FunctionStencilId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirClassMethod {
    pub span: SourceSpan,
    pub key: HirObjectPropertyKey,
    pub function: FunctionStencilId,
    pub is_static: bool,
    pub kind: HirClassMethodKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirClassField {
    pub span: SourceSpan,
    pub key: HirObjectPropertyKey,
    pub initializer: Option<FunctionStencilId>,
    pub is_static: bool,
    pub infer_name: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirClassMethodKind {
    Method,
    Getter,
    Setter,
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
    PrivateMember {
        object: Box<HirExpression>,
        name: HirPrivateName,
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
    BigInt(Arc<str>),
    String(Arc<[u16]>),
    RegExp {
        pattern: Arc<str>,
        flags: u8,
    },
    Boolean(bool),
    Null,
    Identifier(HirIdentifierReference),
    Function(FunctionStencilId),
    Class(HirClass),
    This,
    NewTarget,
    Yield {
        argument: Option<Box<HirExpression>>,
        delegate: bool,
    },
    Await(Box<HirExpression>),
    Sequence(Arc<[HirExpression]>),
    Object(Arc<[HirObjectProperty]>),
    ObjectSpread(Arc<[HirObjectExpressionPart]>),
    Array(Arc<[HirObjectProperty]>),
    ArrayAccumulation(Arc<[HirArrayExpressionPart]>),
    StaticMember {
        object: Box<HirExpression>,
        property: Arc<str>,
    },
    ComputedMember {
        object: Box<HirExpression>,
        property: Box<HirExpression>,
    },
    PrivateMember {
        object: Box<HirExpression>,
        name: HirPrivateName,
    },
    PrivateIn {
        name: HirPrivateName,
        object: Box<HirExpression>,
    },
    SuperStaticMember(Arc<str>),
    SuperComputedMember(Box<HirExpression>),
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
    /// A call whose ordered argument list contains at least one iterator spread.
    CallSpread {
        callee: Box<HirExpression>,
        arguments: Arc<[HirArrayExpressionPart]>,
    },
    SuperCall(Arc<[HirExpression]>),
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
        Expression::BigIntLiteral(literal) => {
            HirExpressionKind::BigInt(Arc::from(literal.value.as_str()))
        }
        Expression::StringLiteral(literal) => {
            HirExpressionKind::String(super::copy_string_literal(literal, source)?)
        }
        Expression::TemplateLiteral(template) => {
            let first = template
                .quasis
                .first()
                .and_then(|quasi| quasi.value.cooked.as_ref())
                .ok_or_else(|| unsupported(source.name(), span, "template literal"))?;
            let mut accumulated = HirExpression {
                span,
                kind: HirExpressionKind::String(super::copy_oxc_string_units(
                    first.as_str(),
                    template.quasis[0].lone_surrogates,
                    source,
                    source_span(template.quasis[0].span),
                )?),
            };
            for (expression, quasi) in template
                .expressions
                .iter()
                .zip(template.quasis.iter().skip(1))
            {
                let expression = lower_expression(expression, source, semantic, functions)?;
                accumulated = HirExpression {
                    span,
                    kind: HirExpressionKind::Binary {
                        operator: HirBinaryOperator::Add,
                        left: Box::new(accumulated),
                        right: Box::new(expression),
                    },
                };
                let cooked = quasi
                    .value
                    .cooked
                    .as_ref()
                    .ok_or_else(|| unsupported(source.name(), span, "template literal"))?;
                if !cooked.is_empty() {
                    accumulated = HirExpression {
                        span,
                        kind: HirExpressionKind::Binary {
                            operator: HirBinaryOperator::Add,
                            left: Box::new(accumulated),
                            right: Box::new(HirExpression {
                                span,
                                kind: HirExpressionKind::String(super::copy_oxc_string_units(
                                    cooked.as_str(),
                                    quasi.lone_surrogates,
                                    source,
                                    source_span(quasi.span),
                                )?),
                            }),
                        },
                    };
                }
            }
            accumulated.kind
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
        Expression::YieldExpression(expression) => HirExpressionKind::Yield {
            argument: expression
                .argument
                .as_ref()
                .map(|argument| lower_expression(argument, source, semantic, functions))
                .transpose()?
                .map(Box::new),
            delegate: expression.delegate,
        },
        Expression::AwaitExpression(expression) => HirExpressionKind::Await(Box::new(
            lower_expression(&expression.argument, source, semantic, functions)?,
        )),
        Expression::SequenceExpression(sequence) => {
            let mut expressions = Vec::with_capacity(sequence.expressions.len());
            for expression in &sequence.expressions {
                expressions.push(lower_expression(expression, source, semantic, functions)?);
            }
            HirExpressionKind::Sequence(expressions.into())
        }
        Expression::ArrayExpression(array) => {
            if array.elements.iter().any(ArrayExpressionElement::is_spread) {
                let mut parts = Vec::new();
                parts.try_reserve_exact(array.elements.len()).map_err(|_| {
                    CompileError::LoweringCapacityOverflow {
                        collection: "HIR array elements",
                    }
                })?;
                for element in &array.elements {
                    let part = if let Some(value) = element.as_expression() {
                        HirArrayExpressionPart::Element(lower_expression(
                            value, source, semantic, functions,
                        )?)
                    } else if let ArrayExpressionElement::SpreadElement(spread) = element {
                        HirArrayExpressionPart::Spread(lower_expression(
                            &spread.argument,
                            source,
                            semantic,
                            functions,
                        )?)
                    } else {
                        HirArrayExpressionPart::Elision
                    };
                    parts.push(part);
                }
                return Ok(HirExpression {
                    span,
                    kind: HirExpressionKind::ArrayAccumulation(parts.into()),
                });
            }
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
                debug_assert!(!element.is_spread(), "spread literals return above");
                chunk_length += 1;
            }
            lower_array_chunk(chunk_properties, chunk_length, span).kind
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
                    match &property.key {
                        PropertyKey::StaticIdentifier(identifier) => {
                            HirObjectPropertyKey::Static(Arc::from(identifier.name.as_str()))
                        }
                        PropertyKey::StringLiteral(literal) => {
                            if literal.lone_surrogates {
                                HirObjectPropertyKey::Computed(HirExpression {
                                    span: source_span(literal.span),
                                    kind: HirExpressionKind::String(super::copy_string_literal(
                                        literal, source,
                                    )?),
                                })
                            } else {
                                HirObjectPropertyKey::Static(Arc::from(literal.value.as_str()))
                            }
                        }
                        PropertyKey::NumericLiteral(literal) => {
                            let mut buffer = ryu_js::Buffer::new();
                            let key = if literal.value == 0.0 {
                                "0"
                            } else {
                                buffer.format(literal.value)
                            };
                            HirObjectPropertyKey::Static(Arc::from(key))
                        }
                        _ => {
                            return Err(unsupported(
                                source.name(),
                                source_span(property.key.span()),
                                "object property key",
                            ));
                        }
                    }
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
        Expression::StaticMemberExpression(expression)
            if !expression.optional && matches!(expression.object, Expression::Super(_)) =>
        {
            HirExpressionKind::SuperStaticMember(Arc::from(expression.property.name.as_str()))
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
        Expression::ComputedMemberExpression(expression)
            if !expression.optional && matches!(expression.object, Expression::Super(_)) =>
        {
            HirExpressionKind::SuperComputedMember(Box::new(lower_expression(
                &expression.expression,
                source,
                semantic,
                functions,
            )?))
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
        Expression::PrivateFieldExpression(expression) if !expression.optional => {
            HirExpressionKind::PrivateMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                name: private_name_reference(&expression.field, source, semantic)?,
            }
        }
        Expression::PrivateInExpression(expression) => HirExpressionKind::PrivateIn {
            name: private_name_reference(&expression.left, source, semantic)?,
            object: Box::new(lower_expression(
                &expression.right,
                source,
                semantic,
                functions,
            )?),
        },
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
        Expression::CallExpression(expression)
            if !expression.optional && matches!(expression.callee, Expression::Super(_)) =>
        {
            let mut arguments = Vec::with_capacity(expression.arguments.len());
            for argument in &expression.arguments {
                let argument = argument.as_expression().ok_or_else(|| {
                    unsupported(
                        source.name(),
                        source_span(argument.span()),
                        "spread super argument",
                    )
                })?;
                arguments.push(lower_expression(argument, source, semantic, functions)?);
            }
            HirExpressionKind::SuperCall(arguments.into())
        }
        Expression::CallExpression(expression) if !expression.optional => {
            let callee = Box::new(lower_expression(
                &expression.callee,
                source,
                semantic,
                functions,
            )?);
            if expression
                .arguments
                .iter()
                .any(|argument| argument.is_spread())
            {
                let mut arguments = Vec::with_capacity(expression.arguments.len());
                for argument in &expression.arguments {
                    if let Some(expression) = argument.as_expression() {
                        arguments.push(HirArrayExpressionPart::Element(lower_expression(
                            expression, source, semantic, functions,
                        )?));
                    } else if let Argument::SpreadElement(spread) = argument {
                        arguments.push(HirArrayExpressionPart::Spread(lower_expression(
                            &spread.argument,
                            source,
                            semantic,
                            functions,
                        )?));
                    } else {
                        unreachable!("Oxc call arguments are expressions or spread elements");
                    }
                }
                HirExpressionKind::CallSpread {
                    callee,
                    arguments: arguments.into(),
                }
            } else {
                let mut arguments = Vec::with_capacity(expression.arguments.len());
                for argument in &expression.arguments {
                    let argument = argument
                        .as_expression()
                        .expect("non-spread call argument is an expression");
                    arguments.push(lower_expression(argument, source, semantic, functions)?);
                }
                HirExpressionKind::Call {
                    callee,
                    arguments: arguments.into(),
                }
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
        Expression::ClassExpression(class) => {
            HirExpressionKind::Class(lower_class(class, None, source, semantic, functions)?)
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

/// Copies one class into ordered elements and independent field-initializer stencils.
pub(super) fn lower_class(
    class: &oxc::ast::ast::Class<'_>,
    declaration_name: Option<Arc<str>>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirClass, CompileError> {
    if !class.decorators.is_empty()
        || class.super_type_arguments.is_some()
        || !class.implements.is_empty()
        || class.r#abstract
        || class.declare
    {
        return Err(unsupported(
            source.name(),
            source_span(class.span),
            "decorated or TypeScript class",
        ));
    }
    let name_binding = class
        .id
        .as_ref()
        .map(|identifier| new_binding(identifier, source, semantic))
        .transpose()?;
    let class_name = declaration_name
        .clone()
        .or_else(|| name_binding.as_ref().map(|binding| binding.name.clone()));
    let class_scope = class
        .scope_id
        .get()
        .ok_or_else(|| missing_semantic(source, source_span(class.span), "class scope"))?;
    let mut constructor = None;
    for element in &class.body.body {
        let oxc::ast::ast::ClassElement::MethodDefinition(method) = element else {
            continue;
        };
        if method.kind != oxc::ast::ast::MethodDefinitionKind::Constructor {
            continue;
        }
        validate_class_method(method, source)?;
        if method.r#static || constructor.is_some() {
            return Err(unsupported(
                source.name(),
                source_span(method.span),
                "duplicate or static constructor",
            ));
        }
        constructor = Some(lower_function_stencil(
            &method.value,
            class_name.clone(),
            None,
            source,
            semantic,
            functions,
        )?);
    }
    let constructor = match constructor {
        Some(constructor) => constructor,
        None => {
            let id = FunctionStencilId(
                u32::try_from(functions.len()).map_err(|_| CompileError::BindingOverflow)?,
            );
            let scope = class
                .scope_id
                .get()
                .ok_or_else(|| missing_semantic(source, source_span(class.span), "class scope"))?;
            functions.push(HirFunction {
                id,
                span: source_span(class.span),
                name: class_name.clone(),
                self_binding: None,
                parameters: Arc::from([]),
                parameter_initializers: Arc::from([]),
                rest_parameter: None,
                body: Arc::from([]),
                scope: to_scope_id(scope),
                strict: true,
                is_arrow: false,
                lexical_arguments_owner: false,
                kind: if class.super_class.is_some() {
                    super::program::HirFunctionKind::DefaultDerivedConstructor
                } else {
                    super::program::HirFunctionKind::DefaultBaseConstructor
                },
                initialize_instance_elements: false,
            });
            id
        }
    };
    let stencil = functions
        .get_mut(constructor.index() as usize)
        .expect("new class constructor stencil is published at its stable index");
    if stencil.kind == super::program::HirFunctionKind::Ordinary {
        stencil.kind = if class.super_class.is_some() {
            super::program::HirFunctionKind::DerivedClassConstructor
        } else {
            super::program::HirFunctionKind::BaseClassConstructor
        };
    }
    stencil.strict = true;
    let mut private_names = Vec::with_capacity(class.body.body.len());
    for element in &class.body.body {
        let identifier = match element {
            oxc::ast::ast::ClassElement::MethodDefinition(method) => {
                let PropertyKey::PrivateIdentifier(identifier) = &method.key else {
                    continue;
                };
                identifier
            }
            oxc::ast::ast::ClassElement::PropertyDefinition(field) => {
                let PropertyKey::PrivateIdentifier(identifier) = &field.key else {
                    continue;
                };
                identifier
            }
            _ => continue,
        };
        let private_name = private_name_definition(class, identifier, source, semantic)?;
        if private_names
            .iter()
            .any(|existing: &HirPrivateName| existing.id == private_name.id)
        {
            continue;
        }
        private_names.push(private_name);
    }
    let mut elements = Vec::with_capacity(class.body.body.len());
    for element in &class.body.body {
        match element {
            oxc::ast::ast::ClassElement::MethodDefinition(method) => {
                if method.kind == oxc::ast::ast::MethodDefinitionKind::Constructor {
                    continue;
                }
                validate_class_method(method, source)?;
                if let PropertyKey::PrivateIdentifier(identifier) = &method.key {
                    let name = private_name_definition(class, identifier, source, semantic)?;
                    let prefix = match method.kind {
                        oxc::ast::ast::MethodDefinitionKind::Method => "",
                        oxc::ast::ast::MethodDefinitionKind::Get => "get ",
                        oxc::ast::ast::MethodDefinitionKind::Set => "set ",
                        oxc::ast::ast::MethodDefinitionKind::Constructor => unreachable!(),
                    };
                    let function_name: Arc<str> = Arc::from(format!("{prefix}#{}", name.name));
                    let function = lower_function_stencil(
                        &method.value,
                        Some(function_name),
                        None,
                        source,
                        semantic,
                        functions,
                    )?;
                    let stencil = functions
                        .get_mut(function.index() as usize)
                        .expect("new private method stencil remains published");
                    stencil.strict = true;
                    stencil.kind = super::program::HirFunctionKind::ClassMethod;
                    if method.kind == oxc::ast::ast::MethodDefinitionKind::Method {
                        elements.push(HirClassElement::PrivateMethod(HirPrivateMethod {
                            span: source_span(method.span),
                            name,
                            function,
                            is_static: method.r#static,
                        }));
                    } else if let Some(accessor) =
                        elements.iter_mut().find_map(|element| match element {
                            HirClassElement::PrivateAccessor(accessor)
                                if accessor.name.id == name.id
                                    && accessor.is_static == method.r#static =>
                            {
                                Some(accessor)
                            }
                            _ => None,
                        })
                    {
                        if method.kind == oxc::ast::ast::MethodDefinitionKind::Get {
                            accessor.getter = Some(function);
                        } else {
                            accessor.setter = Some(function);
                        }
                    } else {
                        elements.push(HirClassElement::PrivateAccessor(HirPrivateAccessor {
                            span: source_span(method.span),
                            name,
                            getter: (method.kind == oxc::ast::ast::MethodDefinitionKind::Get)
                                .then_some(function),
                            setter: (method.kind == oxc::ast::ast::MethodDefinitionKind::Set)
                                .then_some(function),
                            is_static: method.r#static,
                        }));
                    }
                    continue;
                }
                let key = lower_class_key(
                    &method.key,
                    method.computed,
                    "class method key",
                    source,
                    semantic,
                    functions,
                )?;
                let kind = match method.kind {
                    oxc::ast::ast::MethodDefinitionKind::Method => HirClassMethodKind::Method,
                    oxc::ast::ast::MethodDefinitionKind::Get => HirClassMethodKind::Getter,
                    oxc::ast::ast::MethodDefinitionKind::Set => HirClassMethodKind::Setter,
                    oxc::ast::ast::MethodDefinitionKind::Constructor => unreachable!(),
                };
                let function_name = class_method_name(&key, kind);
                let function = lower_function_stencil(
                    &method.value,
                    function_name,
                    None,
                    source,
                    semantic,
                    functions,
                )?;
                let stencil = functions
                    .get_mut(function.index() as usize)
                    .expect("new class method stencil is published at its stable index");
                stencil.strict = true;
                stencil.kind = super::program::HirFunctionKind::ClassMethod;
                elements.push(HirClassElement::Method(HirClassMethod {
                    span: source_span(method.span),
                    key,
                    function,
                    is_static: method.r#static,
                    kind,
                }));
            }
            oxc::ast::ast::ClassElement::PropertyDefinition(field) => {
                if field.r#type.is_abstract()
                    || !field.decorators.is_empty()
                    || field.type_annotation.is_some()
                    || field.declare
                    || field.r#override
                    || field.optional
                    || field.definite
                    || field.readonly
                    || field.accessibility.is_some()
                {
                    return Err(unsupported(
                        source.name(),
                        source_span(field.span),
                        "TypeScript class field",
                    ));
                }
                let infer_name = field
                    .value
                    .as_ref()
                    .is_some_and(oxc::ast::ast::Expression::is_anonymous_function_definition);
                let initializer = field
                    .value
                    .as_ref()
                    .map(|value| {
                        let value = lower_expression(value, source, semantic, functions)?;
                        let id = FunctionStencilId(
                            u32::try_from(functions.len())
                                .map_err(|_| CompileError::BindingOverflow)?,
                        );
                        functions.push(HirFunction {
                            id,
                            span: source_span(field.span),
                            name: None,
                            self_binding: None,
                            parameters: Arc::from([]),
                            parameter_initializers: Arc::from([]),
                            rest_parameter: None,
                            body: Arc::from([super::statement::HirStatement {
                                span: source_span(field.span),
                                completion: super::program::StatementCompletion::Empty,
                                kind: super::statement::HirStatementKind::Return(Some(value)),
                            }]),
                            scope: to_scope_id(class_scope),
                            strict: true,
                            is_arrow: false,
                            lexical_arguments_owner: false,
                            kind: super::program::HirFunctionKind::ClassFieldInitializer,
                            initialize_instance_elements: false,
                        });
                        Ok(id)
                    })
                    .transpose()?;
                if let PropertyKey::PrivateIdentifier(identifier) = &field.key {
                    let name = private_name_definition(class, identifier, source, semantic)?;
                    elements.push(HirClassElement::PrivateField(HirPrivateField {
                        span: source_span(field.span),
                        name,
                        initializer,
                        is_static: field.r#static,
                    }));
                    continue;
                }
                let key = lower_class_key(
                    &field.key,
                    field.computed,
                    "class field key",
                    source,
                    semantic,
                    functions,
                )?;
                elements.push(HirClassElement::PublicField(HirClassField {
                    span: source_span(field.span),
                    key,
                    initializer,
                    is_static: field.r#static,
                    infer_name,
                }));
            }
            oxc::ast::ast::ClassElement::StaticBlock(block) => {
                let mut body = Vec::with_capacity(block.body.len());
                for statement in &block.body {
                    body.push(lower_statement(
                        statement,
                        source,
                        semantic,
                        functions,
                        StatementContext::FunctionBody,
                    )?);
                }
                let function = FunctionStencilId(
                    u32::try_from(functions.len()).map_err(|_| CompileError::BindingOverflow)?,
                );
                let scope = block.scope_id.get().ok_or_else(|| {
                    missing_semantic(source, source_span(block.span), "class static block scope")
                })?;
                functions.push(HirFunction {
                    id: function,
                    span: source_span(block.span),
                    name: None,
                    self_binding: None,
                    parameters: Arc::from([]),
                    parameter_initializers: Arc::from([]),
                    rest_parameter: None,
                    body: body.into(),
                    scope: to_scope_id(scope),
                    strict: true,
                    is_arrow: false,
                    lexical_arguments_owner: false,
                    kind: super::program::HirFunctionKind::ClassStaticBlock,
                    initialize_instance_elements: false,
                });
                elements.push(HirClassElement::StaticBlock(HirClassStaticBlock {
                    span: source_span(block.span),
                    function,
                }));
            }
            _ => {
                return Err(unsupported(
                    source.name(),
                    source_span(element.span()),
                    "private class element",
                ));
            }
        }
    }
    if elements.iter().any(|element| {
        matches!(element, HirClassElement::PublicField(field) if !field.is_static)
            || matches!(element, HirClassElement::PrivateField(field) if !field.is_static)
            || matches!(element, HirClassElement::PrivateMethod(method) if !method.is_static)
            || matches!(element, HirClassElement::PrivateAccessor(accessor) if !accessor.is_static)
    }) {
        functions
            .get_mut(constructor.index() as usize)
            .expect("class constructor stencil remains at its stable index")
            .initialize_instance_elements = true;
    }
    Ok(HirClass {
        name: class_name,
        name_binding,
        scope: to_scope_id(class_scope),
        super_class: class
            .super_class
            .as_ref()
            .map(|super_class| {
                lower_expression(super_class, source, semantic, functions).map(Box::new)
            })
            .transpose()?,
        constructor,
        private_names: private_names.into(),
        elements: elements.into(),
    })
}

/// Resolves one private declaration to a stable module-local class/element identity.
fn private_name_definition(
    class: &oxc::ast::ast::Class<'_>,
    identifier: &oxc::ast::ast::PrivateIdentifier<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
) -> Result<HirPrivateName, CompileError> {
    let classes = semantic.classes();
    let class_node = class.node_id();
    let (class_ordinal, class_id) = classes
        .iter_enumerated()
        .enumerate()
        .find_map(|(ordinal, (class_id, node))| {
            (*node == class_node).then_some((ordinal, class_id))
        })
        .ok_or_else(|| {
            missing_semantic(
                source,
                source_span(identifier.span),
                "private-name declaring class",
            )
        })?;
    private_name_in_class(class_ordinal, class_id, identifier, source, semantic)
}

/// Resolves a private use through the nearest enclosing class that declares the same spelling.
fn private_name_reference(
    identifier: &oxc::ast::ast::PrivateIdentifier<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
) -> Result<HirPrivateName, CompileError> {
    let classes = semantic.classes();
    let reference_node = identifier.node_id();
    let reference_class = classes
        .iter_enumerated()
        .find_map(|(class_id, _)| {
            classes
                .iter_private_identifiers(class_id)
                .any(|reference| reference.id == reference_node)
                .then_some(class_id)
        })
        .ok_or_else(|| {
            missing_semantic(
                source,
                source_span(identifier.span),
                "private-name reference class",
            )
        })?;
    let owner = classes
        .ancestors(reference_class)
        .find(|class_id| classes.has_private_definition(*class_id, identifier.name))
        .ok_or_else(|| {
            missing_semantic(
                source,
                source_span(identifier.span),
                "private-name declaration",
            )
        })?;
    let class_ordinal = classes
        .iter_enumerated()
        .position(|(class_id, _)| class_id == owner)
        .ok_or_else(|| {
            missing_semantic(
                source,
                source_span(identifier.span),
                "private-name owner ordinal",
            )
        })?;
    private_name_in_class(class_ordinal, owner, identifier, source, semantic)
}

/// Produces the shared identity used by a declaration and all of its lexical references.
fn private_name_in_class(
    class_ordinal: usize,
    class_id: oxc::syntax::class::ClassId,
    identifier: &oxc::ast::ast::PrivateIdentifier<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
) -> Result<HirPrivateName, CompileError> {
    let element = semantic.classes().elements[class_id]
        .iter()
        .position(|element| element.is_private && element.name == identifier.name)
        .ok_or_else(|| {
            missing_semantic(source, source_span(identifier.span), "private-name element")
        })?;
    Ok(HirPrivateName {
        id: HirPrivateNameId {
            class: u32::try_from(class_ordinal).map_err(|_| CompileError::BindingOverflow)?,
            element: u32::try_from(element).map_err(|_| CompileError::BindingOverflow)?,
        },
        name: Arc::from(identifier.name.as_str()),
    })
}

fn validate_class_method(
    method: &oxc::ast::ast::MethodDefinition<'_>,
    source: &SourceText,
) -> Result<(), CompileError> {
    if method.r#type.is_abstract()
        || !method.decorators.is_empty()
        || method.accessibility.is_some()
        || method.r#override
        || method.optional
    {
        return Err(unsupported(
            source.name(),
            source_span(method.span),
            "TypeScript class method",
        ));
    }
    Ok(())
}

fn lower_class_key(
    key: &PropertyKey<'_>,
    computed: bool,
    syntax: &'static str,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirObjectPropertyKey, CompileError> {
    if computed {
        return Ok(HirObjectPropertyKey::Computed(lower_expression(
            key.to_expression(),
            source,
            semantic,
            functions,
        )?));
    }
    let name: Arc<str> = match key {
        PropertyKey::StaticIdentifier(identifier) => Arc::from(identifier.name.as_str()),
        PropertyKey::StringLiteral(literal) => {
            if literal.lone_surrogates {
                return Ok(HirObjectPropertyKey::Computed(HirExpression {
                    span: source_span(literal.span),
                    kind: HirExpressionKind::String(super::copy_string_literal(literal, source)?),
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
        _ => return Err(unsupported(source.name(), source_span(key.span()), syntax)),
    };
    Ok(HirObjectPropertyKey::Static(name))
}

fn class_method_name(key: &HirObjectPropertyKey, kind: HirClassMethodKind) -> Option<Arc<str>> {
    match (key, kind) {
        (HirObjectPropertyKey::Computed(_), _) => None,
        (HirObjectPropertyKey::Static(name), HirClassMethodKind::Method) => Some(name.clone()),
        (HirObjectPropertyKey::Static(name), HirClassMethodKind::Getter) => {
            Some(Arc::from(format!("get {name}")))
        }
        (HirObjectPropertyKey::Static(name), HirClassMethodKind::Setter) => {
            Some(Arc::from(format!("set {name}")))
        }
    }
}

/// Builds one owned ordinary array literal while preserving its trailing sparse length.
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
        AssignmentTarget::PrivateFieldExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::PrivateMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                name: private_name_reference(&expression.field, source, semantic)?,
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
        SimpleAssignmentTarget::PrivateFieldExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::PrivateMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    semantic,
                    functions,
                )?),
                name: private_name_reference(&expression.field, source, semantic)?,
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
    let lexical_arguments_owner = (identifier.name == "arguments"
        && reference.symbol_id().is_none())
    .then(|| super::program::lexical_arguments_owner(semantic, reference.scope_id()))
    .flatten();
    let binding_scope = reference
        .symbol_id()
        .map(|symbol| to_scope_id(semantic.scoping().symbol_scope_id(symbol)))
        .or(lexical_arguments_owner);
    Ok(HirIdentifierReference {
        id: ReferenceId(id.index() as u32),
        scope: to_scope_id(reference.scope_id()),
        binding: reference
            .symbol_id()
            .map(|symbol| BindingId(symbol.index() as u32))
            .or_else(|| lexical_arguments_owner.map(super::program::lexical_arguments_binding)),
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
