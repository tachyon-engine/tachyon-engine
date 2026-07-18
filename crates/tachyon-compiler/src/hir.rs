//! Owned high-level IR that intentionally does not retain Oxc AST nodes or arena lifetimes.

use std::sync::Arc;

use oxc::{
    ast::ast::{
        AssignmentTarget, BindingPattern, Expression, ForStatementInit, Program,
        SimpleAssignmentTarget, Statement, VariableDeclaration, VariableDeclarationKind,
    },
    span::{GetSpan, Span},
    syntax::operator::{
        AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
    },
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
    Block(Arc<[HirStatement]>),
    If {
        test: HirExpression,
        consequent: Box<HirStatement>,
        alternate: Option<Box<HirStatement>>,
    },
    For {
        initializer: Option<HirForInitializer>,
        test: Option<HirExpression>,
        update: Option<HirExpression>,
        body: Box<HirStatement>,
    },
    Switch {
        discriminant: HirExpression,
        cases: Arc<[HirSwitchCase]>,
    },
    Try {
        block: Arc<[HirStatement]>,
        handler: Option<HirCatchClause>,
        finalizer: Option<Arc<[HirStatement]>>,
    },
    Break,
    Continue,
    Return(Option<HirExpression>),
    Throw(HirExpression),
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirForInitializer {
    Variable(HirVariableDeclaration),
    Expression(HirExpression),
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirCatchClause {
    pub span: SourceSpan,
    pub parameter: Option<HirBinding>,
    pub body: Arc<[HirStatement]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirSwitchCase {
    pub span: SourceSpan,
    pub test: Option<HirExpression>,
    pub consequent: Arc<[HirStatement]>,
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
    pub name: Option<Arc<str>>,
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
pub enum HirAssignmentTarget {
    Identifier(Arc<str>),
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpressionKind {
    Number(u64),
    String(Arc<str>),
    Boolean(bool),
    Null,
    Identifier(Arc<str>),
    Function(FunctionStencilId),
    This,
    NewTarget,
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
        target: HirAssignmentTarget,
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

/// Lowers only the currently supported Oxc subset and copies every retained field into owned HIR.
pub(crate) fn lower(
    program: &Program<'_>,
    kind: ProgramKind,
    source: &SourceText,
) -> Result<HirProgram, CompileError> {
    let mut statements = Vec::with_capacity(program.body.len());
    let mut functions = Vec::with_capacity(program.body.len());
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
            kind: HirStatementKind::Expression(lower_expression(
                &statement.expression,
                source,
                next_binding,
                functions,
            )?),
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
                functions,
            )?),
        }),
        Statement::FunctionDeclaration(function) if allow_function_declaration => {
            lower_function_declaration(function, source, next_binding, functions)
        }
        Statement::BlockStatement(block) => {
            let mut statements = Vec::with_capacity(block.body.len());
            for statement in &block.body {
                statements.push(lower_statement(
                    statement,
                    source,
                    next_binding,
                    functions,
                    false,
                )?);
            }
            Ok(HirStatement {
                span: source_span(block.span),
                completion: StatementCompletion::Empty,
                kind: HirStatementKind::Block(statements.into()),
            })
        }
        Statement::IfStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::If {
                test: lower_expression(&statement.test, source, next_binding, functions)?,
                consequent: Box::new(lower_statement(
                    &statement.consequent,
                    source,
                    next_binding,
                    functions,
                    false,
                )?),
                alternate: statement
                    .alternate
                    .as_ref()
                    .map(|alternate| {
                        lower_statement(alternate, source, next_binding, functions, false)
                            .map(Box::new)
                    })
                    .transpose()?,
            },
        }),
        Statement::ForStatement(statement) => {
            lower_for_statement(statement, source, next_binding, functions)
        }
        Statement::SwitchStatement(statement) => {
            lower_switch_statement(statement, source, next_binding, functions)
        }
        Statement::TryStatement(statement) => {
            lower_try_statement(statement, source, next_binding, functions)
        }
        Statement::BreakStatement(statement) if statement.label.is_none() => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Break,
        }),
        Statement::BreakStatement(statement) => Err(unsupported(
            source.name(),
            source_span(statement.span),
            "labelled break",
        )),
        Statement::ContinueStatement(statement) if statement.label.is_none() => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Continue,
        }),
        Statement::ContinueStatement(statement) => Err(unsupported(
            source.name(),
            source_span(statement.span),
            "labelled continue",
        )),
        Statement::ReturnStatement(statement) if !allow_function_declaration => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Return(
                statement
                    .argument
                    .as_ref()
                    .map(|argument| lower_expression(argument, source, next_binding, functions))
                    .transpose()?,
            ),
        }),
        Statement::ThrowStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Throw(lower_expression(
                &statement.argument,
                source,
                next_binding,
                functions,
            )?),
        }),
        _ => Err(unsupported(
            source.name(),
            source_span(statement.span()),
            "statement",
        )),
    }
}

/// Owns a classic for-loop without collapsing its update target or continue destination.
fn lower_for_statement(
    statement: &oxc::ast::ast::ForStatement<'_>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<HirStatement, CompileError> {
    let initializer = statement
        .init
        .as_ref()
        .map(|initializer| match initializer {
            ForStatementInit::VariableDeclaration(declaration) => {
                lower_variable_declaration(declaration, source, next_binding, functions)
                    .map(HirForInitializer::Variable)
            }
            initializer => {
                lower_expression(initializer.to_expression(), source, next_binding, functions)
                    .map(HirForInitializer::Expression)
            }
        })
        .transpose()?;
    Ok(HirStatement {
        span: source_span(statement.span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::For {
            initializer,
            test: statement
                .test
                .as_ref()
                .map(|test| lower_expression(test, source, next_binding, functions))
                .transpose()?,
            update: statement
                .update
                .as_ref()
                .map(|update| lower_expression(update, source, next_binding, functions))
                .transpose()?,
            body: Box::new(lower_statement(
                &statement.body,
                source,
                next_binding,
                functions,
                false,
            )?),
        },
    })
}

/// Owns try/catch/finally bodies while restricting the first catch slice to identifier binding.
fn lower_try_statement(
    statement: &oxc::ast::ast::TryStatement<'_>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<HirStatement, CompileError> {
    let mut block = Vec::with_capacity(statement.block.body.len());
    for statement in &statement.block.body {
        block.push(lower_statement(
            statement,
            source,
            next_binding,
            functions,
            false,
        )?);
    }
    let handler = statement
        .handler
        .as_ref()
        .map(|handler| {
            let parameter = handler
                .param
                .as_ref()
                .map(|parameter| match &parameter.pattern {
                    BindingPattern::BindingIdentifier(identifier) => new_binding(
                        identifier.name.as_str(),
                        source_span(identifier.span),
                        next_binding,
                    ),
                    _ => Err(unsupported(
                        source.name(),
                        source_span(parameter.span),
                        "catch binding pattern",
                    )),
                })
                .transpose()?;
            let mut body = Vec::with_capacity(handler.body.body.len());
            for statement in &handler.body.body {
                body.push(lower_statement(
                    statement,
                    source,
                    next_binding,
                    functions,
                    false,
                )?);
            }
            Ok(HirCatchClause {
                span: source_span(handler.span),
                parameter,
                body: body.into(),
            })
        })
        .transpose()?;
    let finalizer = statement
        .finalizer
        .as_ref()
        .map(|finalizer| {
            let mut statements = Vec::with_capacity(finalizer.body.len());
            for statement in &finalizer.body {
                statements.push(lower_statement(
                    statement,
                    source,
                    next_binding,
                    functions,
                    false,
                )?);
            }
            Ok::<Arc<[HirStatement]>, CompileError>(statements.into())
        })
        .transpose()?;
    Ok(HirStatement {
        span: source_span(statement.span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::Try {
            block: block.into(),
            handler,
            finalizer,
        },
    })
}

/// Copies switch clause order exactly because default placement controls subsequent fallthrough.
fn lower_switch_statement(
    statement: &oxc::ast::ast::SwitchStatement<'_>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<HirStatement, CompileError> {
    let mut cases = Vec::with_capacity(statement.cases.len());
    for case in &statement.cases {
        let mut consequent = Vec::with_capacity(case.consequent.len());
        for statement in &case.consequent {
            consequent.push(lower_statement(
                statement,
                source,
                next_binding,
                functions,
                false,
            )?);
        }
        cases.push(HirSwitchCase {
            span: source_span(case.span),
            test: case
                .test
                .as_ref()
                .map(|test| lower_expression(test, source, next_binding, functions))
                .transpose()?,
            consequent: consequent.into(),
        });
    }
    Ok(HirStatement {
        span: source_span(statement.span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::Switch {
            discriminant: lower_expression(
                &statement.discriminant,
                source,
                next_binding,
                functions,
            )?,
            cases: cases.into(),
        },
    })
}

/// Copies one ordinary declaration and its simple parameters/body into an owned function stencil.
fn lower_function_declaration(
    function: &oxc::ast::ast::Function<'_>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<HirStatement, CompileError> {
    let identifier = function.id.as_ref().ok_or_else(|| {
        unsupported(
            source.name(),
            source_span(function.span),
            "anonymous function declaration",
        )
    })?;
    let declaration_binding = new_binding(
        identifier.name.as_str(),
        source_span(identifier.span),
        next_binding,
    )?;
    let id = lower_function_stencil(
        function,
        Some(Arc::from(identifier.name.as_str())),
        source,
        next_binding,
        functions,
    )?;
    Ok(HirStatement {
        span: source_span(function.span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::FunctionDeclaration(HirFunctionDeclaration {
            binding: declaration_binding,
            function: id,
        }),
    })
}

/// Copies parameters and body into the next stable stencil after nested functions are lowered.
fn lower_function_stencil(
    function: &oxc::ast::ast::Function<'_>,
    name: Option<Arc<str>>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<FunctionStencilId, CompileError> {
    if function.generator || function.r#async || function.params.rest.is_some() {
        return Err(unsupported(
            source.name(),
            source_span(function.span),
            "generator/async/rest function",
        ));
    }
    let body = function.body.as_ref().ok_or_else(|| {
        unsupported(
            source.name(),
            source_span(function.span),
            "function without body",
        )
    })?;
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
        name,
        parameters: parameters.into(),
        body: statements.into(),
    });
    Ok(id)
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
    functions: &mut Vec<HirFunction>,
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
                .map(|initializer| lower_expression(initializer, source, next_binding, functions))
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
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
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
        Expression::FunctionExpression(function) if function.id.is_none() => {
            HirExpressionKind::Function(lower_function_stencil(
                function,
                None,
                source,
                next_binding,
                functions,
            )?)
        }
        Expression::FunctionExpression(_) => {
            return Err(unsupported(
                source.name(),
                span,
                "named function expression",
            ));
        }
        Expression::ThisExpression(_) => HirExpressionKind::This,
        Expression::MetaProperty(property)
            if property.meta.name == "new" && property.property.name == "target" =>
        {
            HirExpressionKind::NewTarget
        }
        Expression::StaticMemberExpression(expression) if !expression.optional => {
            HirExpressionKind::StaticMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    next_binding,
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
                    next_binding,
                    functions,
                )?),
                property: Box::new(lower_expression(
                    &expression.expression,
                    source,
                    next_binding,
                    functions,
                )?),
            }
        }
        Expression::UnaryExpression(expression) => HirExpressionKind::Unary {
            operator: lower_unary_operator(expression.operator),
            argument: Box::new(lower_expression(
                &expression.argument,
                source,
                next_binding,
                functions,
            )?),
        },
        Expression::BinaryExpression(expression) => HirExpressionKind::Binary {
            operator: lower_binary_operator(expression.operator),
            left: Box::new(lower_expression(
                &expression.left,
                source,
                next_binding,
                functions,
            )?),
            right: Box::new(lower_expression(
                &expression.right,
                source,
                next_binding,
                functions,
            )?),
        },
        Expression::LogicalExpression(expression) => HirExpressionKind::Logical {
            operator: lower_logical_operator(expression.operator),
            left: Box::new(lower_expression(
                &expression.left,
                source,
                next_binding,
                functions,
            )?),
            right: Box::new(lower_expression(
                &expression.right,
                source,
                next_binding,
                functions,
            )?),
        },
        Expression::AssignmentExpression(expression) => HirExpressionKind::Assignment {
            operator: lower_assignment_operator(expression.operator, source, expression.span)?,
            target: lower_assignment_target(&expression.left, source, next_binding, functions)?,
            value: Box::new(lower_expression(
                &expression.right,
                source,
                next_binding,
                functions,
            )?),
        },
        Expression::UpdateExpression(expression) => HirExpressionKind::Update {
            operator: match expression.operator {
                UpdateOperator::Increment => HirUpdateOperator::Increment,
                UpdateOperator::Decrement => HirUpdateOperator::Decrement,
            },
            prefix: expression.prefix,
            target: lower_update_target(&expression.argument, source, next_binding, functions)?,
        },
        Expression::ConditionalExpression(expression) => HirExpressionKind::Conditional {
            test: Box::new(lower_expression(
                &expression.test,
                source,
                next_binding,
                functions,
            )?),
            consequent: Box::new(lower_expression(
                &expression.consequent,
                source,
                next_binding,
                functions,
            )?),
            alternate: Box::new(lower_expression(
                &expression.alternate,
                source,
                next_binding,
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
                arguments.push(lower_expression(argument, source, next_binding, functions)?);
            }
            HirExpressionKind::Call {
                callee: Box::new(lower_expression(
                    &expression.callee,
                    source,
                    next_binding,
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
                arguments.push(lower_expression(argument, source, next_binding, functions)?);
            }
            HirExpressionKind::New {
                callee: Box::new(lower_expression(
                    &expression.callee,
                    source,
                    next_binding,
                    functions,
                )?),
                arguments: arguments.into(),
            }
        }
        Expression::ParenthesizedExpression(expression) => {
            let mut lowered =
                lower_expression(&expression.expression, source, next_binding, functions)?;
            lowered.span = span;
            return Ok(lowered);
        }
        _ => return Err(unsupported(source.name(), span, "expression")),
    };
    Ok(HirExpression { span, kind })
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
    operator
        .to_binary_operator()
        .map(lower_binary_operator)
        .map(HirAssignmentOperator::Binary)
        .ok_or_else(|| unsupported(source.name(), source_span(span), "assignment operator"))
}

/// Owns identifier and static-member references while rejecting patterns and computed properties.
fn lower_assignment_target(
    target: &AssignmentTarget<'_>,
    source: &SourceText,
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<HirAssignmentTarget, CompileError> {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(identifier) => Ok(
            HirAssignmentTarget::Identifier(Arc::from(identifier.name.as_str())),
        ),
        AssignmentTarget::StaticMemberExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::StaticMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    next_binding,
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
                    next_binding,
                    functions,
                )?),
                property: Box::new(lower_expression(
                    &expression.expression,
                    source,
                    next_binding,
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
    next_binding: &mut u32,
    functions: &mut Vec<HirFunction>,
) -> Result<HirAssignmentTarget, CompileError> {
    match target {
        SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => Ok(
            HirAssignmentTarget::Identifier(Arc::from(identifier.name.as_str())),
        ),
        SimpleAssignmentTarget::StaticMemberExpression(expression) if !expression.optional => {
            Ok(HirAssignmentTarget::StaticMember {
                object: Box::new(lower_expression(
                    &expression.object,
                    source,
                    next_binding,
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
                    next_binding,
                    functions,
                )?),
                property: Box::new(lower_expression(
                    &expression.expression,
                    source,
                    next_binding,
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
