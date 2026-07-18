//! Owned high-level IR that intentionally does not retain Oxc AST nodes or arena lifetimes.

use std::sync::Arc;

use oxc::{
    ast::ast::{
        AssignmentTarget, BindingPattern, Expression, Program, Statement, VariableDeclaration,
        VariableDeclarationKind,
    },
    span::GetSpan,
    syntax::operator::{AssignmentOperator, BinaryOperator, UnaryOperator},
};

use crate::{CompileError, ProgramKind, SourceId, SourceName, SourceSpan, SourceText};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ScopeId(u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct BindingId(u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ReferenceId(u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct FunctionStencilId(u32);

impl FunctionStencilId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatementCompletion {
    Value,
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirProgram {
    source_id: SourceId,
    kind: ProgramKind,
    statements: Arc<[HirStatement]>,
    functions: Arc<[HirFunction]>,
}

impl HirProgram {
    #[must_use]
    pub const fn source_id(&self) -> SourceId {
        self.source_id
    }

    #[must_use]
    pub const fn kind(&self) -> ProgramKind {
        self.kind
    }

    #[must_use]
    pub fn statements(&self) -> &[HirStatement] {
        &self.statements
    }

    #[must_use]
    pub fn functions(&self) -> &[HirFunction] {
        &self.functions
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirStatement {
    pub span: SourceSpan,
    pub completion: StatementCompletion,
    pub kind: HirStatementKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirStatementKind {
    Expression(HirExpression),
    VariableDeclaration(HirVariableDeclaration),
    FunctionDeclaration(HirFunctionDeclaration),
    Return(Option<HirExpression>),
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirFunctionDeclaration {
    pub binding: HirBinding,
    pub function: FunctionStencilId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirFunction {
    pub id: FunctionStencilId,
    pub span: SourceSpan,
    pub name: Arc<str>,
    pub parameters: Arc<[HirBinding]>,
    pub body: Arc<[HirStatement]>,
}

/// An owned lexical binding declaration, independent from Oxc's arena-backed identifier node.
#[derive(Clone, Debug, PartialEq)]
pub struct HirBinding {
    pub id: BindingId,
    pub span: SourceSpan,
    pub name: Arc<str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirVariableDeclarator {
    pub span: SourceSpan,
    pub binding: HirBinding,
    pub initializer: Option<HirExpression>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirVariableDeclarationKind {
    Var,
    Let,
    Const,
    Using,
    AwaitUsing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirVariableDeclaration {
    pub kind: HirVariableDeclarationKind,
    pub declarators: Arc<[HirVariableDeclarator]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirExpression {
    pub span: SourceSpan,
    pub kind: HirExpressionKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpressionKind {
    Number(u64),
    String(Arc<str>),
    Boolean(bool),
    Null,
    Identifier(Arc<str>),
    Unary {
        operator: HirUnaryOperator,
        argument: Box<HirExpression>,
    },
    Binary {
        operator: HirBinaryOperator,
        left: Box<HirExpression>,
        right: Box<HirExpression>,
    },
    Assignment {
        target: Arc<str>,
        value: Box<HirExpression>,
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

/// Lowers only the currently supported Oxc subset and copies every retained field into owned HIR.
pub(crate) fn lower(
    program: &Program<'_>,
    kind: ProgramKind,
    source: &SourceText,
) -> Result<HirProgram, CompileError> {
    let mut statements = Vec::with_capacity(program.body.len());
    let mut functions = Vec::new();
    let mut next_binding = 0;
    for statement in &program.body {
        statements.push(lower_statement(
            statement,
            source,
            &mut next_binding,
            &mut functions,
            true,
        )?);
    }
    Ok(HirProgram {
        source_id: source.id(),
        kind,
        statements: statements.into(),
        functions: functions.into(),
    })
}

/// Copies one statement while enforcing which declaration/control forms are legal in this scope.
fn lower_statement(
    statement: &Statement<'_>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
    allow_function_declaration: bool,
) -> Result<HirStatement, CompileError> {
    match statement {
        Statement::ExpressionStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Value,
            kind: HirStatementKind::Expression(lower_expression(&statement.expression, source)?),
        }),
        Statement::EmptyStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Empty,
        }),
        Statement::VariableDeclaration(declaration) => Ok(HirStatement {
            span: source_span(declaration.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::VariableDeclaration(lower_variable_declaration(
                declaration,
                source,
                next_binding,
            )?),
        }),
        Statement::FunctionDeclaration(function) if allow_function_declaration => {
            lower_function_declaration(function, source, next_binding, functions)
        }
        Statement::ReturnStatement(statement) if !allow_function_declaration => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Return(
                statement
                    .argument
                    .as_ref()
                    .map(|argument| lower_expression(argument, source))
                    .transpose()?,
            ),
        }),
        _ => Err(unsupported(
            source.name(),
            source_span(statement.span()),
            "statement",
        )),
    }
}

/// Copies one ordinary declaration and its simple parameters/body into an owned function stencil.
fn lower_function_declaration(
    function: &oxc::ast::ast::Function<'_>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<HirStatement, CompileError> {
    if function.generator || function.r#async || function.params.rest.is_some() {
        return Err(unsupported(
            source.name(),
            source_span(function.span),
            "generator/async/rest function declaration",
        ));
    }
    let identifier = function.id.as_ref().ok_or_else(|| {
        unsupported(
            source.name(),
            source_span(function.span),
            "anonymous function declaration",
        )
    })?;
    let body = function.body.as_ref().ok_or_else(|| {
        unsupported(
            source.name(),
            source_span(function.span),
            "function declaration without body",
        )
    })?;
    let declaration_binding = new_binding(
        identifier.name.as_str(),
        source_span(identifier.span),
        next_binding,
    )?;
    let mut parameters = Vec::with_capacity(function.params.items.len());
    for parameter in &function.params.items {
        if parameter.initializer.is_some() {
            return Err(unsupported(
                source.name(),
                source_span(parameter.span),
                "default parameter",
            ));
        }
        let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
            return Err(unsupported(
                source.name(),
                source_span(parameter.pattern.span()),
                "parameter binding pattern",
            ));
        };
        parameters.push(new_binding(
            identifier.name.as_str(),
            source_span(identifier.span),
            next_binding,
        )?);
    }
    let mut statements = Vec::with_capacity(body.statements.len());
    for statement in &body.statements {
        statements.push(lower_statement(
            statement,
            source,
            next_binding,
            functions,
            false,
        )?);
    }
    let id = FunctionStencilId(
        u32::try_from(functions.len()).map_err(|_| CompileError::BindingOverflow)?,
    );
    functions.push(HirFunction {
        id,
        span: source_span(function.span),
        name: Arc::from(identifier.name.as_str()),
        parameters: parameters.into(),
        body: statements.into(),
    });
    Ok(HirStatement {
        span: source_span(function.span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::FunctionDeclaration(HirFunctionDeclaration {
            binding: declaration_binding,
            function: id,
        }),
    })
}

fn new_binding(
    name: &str,
    span: SourceSpan,
    next_binding: &mut u32,
) -> Result<HirBinding, CompileError> {
    let binding = HirBinding {
        id: BindingId(*next_binding),
        span,
        name: Arc::from(name),
    };
    *next_binding = next_binding
        .checked_add(1)
        .ok_or(CompileError::BindingOverflow)?;
    Ok(binding)
}

/// Copies simple variable declarations and assigns stable IDs before the Oxc arena is discarded.
fn lower_variable_declaration(
    declaration: &VariableDeclaration<'_>,
    source: &SourceText,
    next_binding: &mut u32,
) -> Result<HirVariableDeclaration, CompileError> {
    let mut declarators = Vec::with_capacity(declaration.declarations.len());
    for declarator in &declaration.declarations {
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return Err(unsupported(
                source.name(),
                source_span(declarator.id.span()),
                "binding pattern",
            ));
        };
        let binding = new_binding(
            identifier.name.as_str(),
            source_span(identifier.span),
            next_binding,
        )?;
        declarators.push(HirVariableDeclarator {
            span: source_span(declarator.span),
            binding,
            initializer: declarator
                .init
                .as_ref()
                .map(|initializer| lower_expression(initializer, source))
                .transpose()?,
        });
    }
    Ok(HirVariableDeclaration {
        kind: lower_variable_declaration_kind(declaration.kind),
        declarators: declarators.into(),
    })
}

fn lower_variable_declaration_kind(kind: VariableDeclarationKind) -> HirVariableDeclarationKind {
    match kind {
        VariableDeclarationKind::Var => HirVariableDeclarationKind::Var,
        VariableDeclarationKind::Let => HirVariableDeclarationKind::Let,
        VariableDeclarationKind::Const => HirVariableDeclarationKind::Const,
        VariableDeclarationKind::Using => HirVariableDeclarationKind::Using,
        VariableDeclarationKind::AwaitUsing => HirVariableDeclarationKind::AwaitUsing,
    }
}

/// Recursively copies leaf values and operands so the returned expression has no arena-backed memory.
fn lower_expression(
    expression: &Expression<'_>,
    source: &SourceText,
) -> Result<HirExpression, CompileError> {
    let span = source_span(expression.span());
    let kind = match expression {
        Expression::NumericLiteral(literal) => HirExpressionKind::Number(literal.value.to_bits()),
        Expression::StringLiteral(literal) => {
            HirExpressionKind::String(Arc::from(literal.value.as_str()))
        }
        Expression::BooleanLiteral(literal) => HirExpressionKind::Boolean(literal.value),
        Expression::NullLiteral(_) => HirExpressionKind::Null,
        Expression::Identifier(identifier) => {
            HirExpressionKind::Identifier(Arc::from(identifier.name.as_str()))
        }
        Expression::UnaryExpression(expression) => HirExpressionKind::Unary {
            operator: lower_unary_operator(expression.operator),
            argument: Box::new(lower_expression(&expression.argument, source)?),
        },
        Expression::BinaryExpression(expression) => HirExpressionKind::Binary {
            operator: lower_binary_operator(expression.operator),
            left: Box::new(lower_expression(&expression.left, source)?),
            right: Box::new(lower_expression(&expression.right, source)?),
        },
        Expression::AssignmentExpression(expression) => HirExpressionKind::Assignment {
            target: lower_assignment_target(&expression.left, expression.operator, source)?,
            value: Box::new(lower_expression(&expression.right, source)?),
        },
        Expression::ConditionalExpression(expression) => HirExpressionKind::Conditional {
            test: Box::new(lower_expression(&expression.test, source)?),
            consequent: Box::new(lower_expression(&expression.consequent, source)?),
            alternate: Box::new(lower_expression(&expression.alternate, source)?),
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
                arguments.push(lower_expression(argument, source)?);
            }
            HirExpressionKind::Call {
                callee: Box::new(lower_expression(&expression.callee, source)?),
                arguments: arguments.into(),
            }
        }
        Expression::ParenthesizedExpression(expression) => {
            let mut lowered = lower_expression(&expression.expression, source)?;
            lowered.span = span;
            return Ok(lowered);
        }
        _ => return Err(unsupported(source.name(), span, "expression")),
    };
    Ok(HirExpression { span, kind })
}

/// Retains only an identifier target because member and pattern assignments need object/iterator semantics.
fn lower_assignment_target(
    target: &AssignmentTarget<'_>,
    operator: AssignmentOperator,
    source: &SourceText,
) -> Result<Arc<str>, CompileError> {
    if !operator.is_assign() {
        return Err(unsupported(
            source.name(),
            source_span(target.span()),
            "assignment operator",
        ));
    }
    let AssignmentTarget::AssignmentTargetIdentifier(identifier) = target else {
        return Err(unsupported(
            source.name(),
            source_span(target.span()),
            "assignment target",
        ));
    };
    Ok(Arc::from(identifier.name.as_str()))
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
