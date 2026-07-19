use std::sync::Arc;

use oxc::{
    ast::ast::{
        BindingPattern, ForStatementInit, ForStatementLeft, Statement, VariableDeclaration,
        VariableDeclarationKind,
    },
    semantic::{ScopeFlags as OxcScopeFlags, Semantic},
    span::GetSpan,
};

use crate::{CompileError, SourceSpan, SourceText};

use super::expression::{
    HirAssignmentTarget, HirExpression, lower_assignment_target, lower_expression,
};
use super::program::{
    BindingId, FunctionStencilId, HirBinding, HirFunction, HirFunctionDeclaration,
    StatementCompletion,
};
use super::{missing_semantic, source_span, to_scope_id, unsupported};

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
    ForIn {
        left: HirForInLeft,
        right: HirExpression,
        body: Box<HirStatement>,
    },
    Loop {
        test: HirExpression,
        body: Box<HirStatement>,
        test_first: bool,
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
pub enum HirForInLeft {
    Variable(HirVariableDeclaration),
    Assignment(HirAssignmentTarget),
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

/// Copies one statement while enforcing which declaration/control forms are legal in this scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatementContext {
    ScriptBody,
    ScriptNested,
    FunctionBody,
    FunctionNested,
}

impl StatementContext {
    const fn nested(self) -> Self {
        match self {
            Self::ScriptBody | Self::ScriptNested => Self::ScriptNested,
            Self::FunctionBody | Self::FunctionNested => Self::FunctionNested,
        }
    }

    const fn allows_return(self) -> bool {
        matches!(self, Self::FunctionBody | Self::FunctionNested)
    }
}

pub(super) fn lower_statement(
    statement: &Statement<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
    context: StatementContext,
) -> Result<HirStatement, CompileError> {
    match statement {
        Statement::ExpressionStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Value,
            kind: HirStatementKind::Expression(lower_expression(
                &statement.expression,
                source,
                semantic,
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
                semantic,
                functions,
            )?),
        }),
        Statement::FunctionDeclaration(function)
            if matches!(
                context,
                StatementContext::ScriptBody | StatementContext::FunctionBody
            ) =>
        {
            lower_function_declaration(function, source, semantic, functions)
        }
        Statement::BlockStatement(block) => {
            let mut statements = Vec::with_capacity(block.body.len());
            for statement in &block.body {
                statements.push(lower_statement(
                    statement,
                    source,
                    semantic,
                    functions,
                    context.nested(),
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
                test: lower_expression(&statement.test, source, semantic, functions)?,
                consequent: Box::new(lower_statement(
                    &statement.consequent,
                    source,
                    semantic,
                    functions,
                    context.nested(),
                )?),
                alternate: statement
                    .alternate
                    .as_ref()
                    .map(|alternate| {
                        lower_statement(alternate, source, semantic, functions, context.nested())
                            .map(Box::new)
                    })
                    .transpose()?,
            },
        }),
        Statement::ForStatement(statement) => {
            lower_for_statement(statement, source, semantic, functions, context)
        }
        Statement::ForInStatement(statement) => {
            lower_for_in_statement(statement, source, semantic, functions, context)
        }
        Statement::WhileStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Loop {
                test: lower_expression(&statement.test, source, semantic, functions)?,
                body: Box::new(lower_statement(
                    &statement.body,
                    source,
                    semantic,
                    functions,
                    context.nested(),
                )?),
                test_first: true,
            },
        }),
        Statement::DoWhileStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Loop {
                test: lower_expression(&statement.test, source, semantic, functions)?,
                body: Box::new(lower_statement(
                    &statement.body,
                    source,
                    semantic,
                    functions,
                    context.nested(),
                )?),
                test_first: false,
            },
        }),
        Statement::SwitchStatement(statement) => {
            lower_switch_statement(statement, source, semantic, functions, context)
        }
        Statement::TryStatement(statement) => {
            lower_try_statement(statement, source, semantic, functions, context)
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
        Statement::ReturnStatement(statement) if context.allows_return() => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Return(
                statement
                    .argument
                    .as_ref()
                    .map(|argument| lower_expression(argument, source, semantic, functions))
                    .transpose()?,
            ),
        }),
        Statement::ThrowStatement(statement) => Ok(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::Throw(lower_expression(
                &statement.argument,
                source,
                semantic,
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

/// Owns one for-in target while rejecting initializers and destructuring until iteration envs land.
fn lower_for_in_statement(
    statement: &oxc::ast::ast::ForInStatement<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
    context: StatementContext,
) -> Result<HirStatement, CompileError> {
    let left = match &statement.left {
        ForStatementLeft::VariableDeclaration(declaration) => {
            let declaration = lower_variable_declaration(declaration, source, semantic, functions)?;
            if declaration.declarators.len() != 1
                || declaration.declarators[0].initializer.is_some()
            {
                return Err(unsupported(
                    source.name(),
                    source_span(statement.left.span()),
                    "for-in declaration",
                ));
            }
            HirForInLeft::Variable(declaration)
        }
        left => {
            let target = left.as_assignment_target().ok_or_else(|| {
                unsupported(
                    source.name(),
                    source_span(statement.left.span()),
                    "for-in assignment target",
                )
            })?;
            HirForInLeft::Assignment(lower_assignment_target(
                target, source, semantic, functions,
            )?)
        }
    };
    Ok(HirStatement {
        span: source_span(statement.span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::ForIn {
            left,
            right: lower_expression(&statement.right, source, semantic, functions)?,
            body: Box::new(lower_statement(
                &statement.body,
                source,
                semantic,
                functions,
                context.nested(),
            )?),
        },
    })
}

/// Owns a classic for-loop without collapsing its update target or continue destination.
fn lower_for_statement(
    statement: &oxc::ast::ast::ForStatement<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
    context: StatementContext,
) -> Result<HirStatement, CompileError> {
    let initializer = statement
        .init
        .as_ref()
        .map(|initializer| match initializer {
            ForStatementInit::VariableDeclaration(declaration) => {
                lower_variable_declaration(declaration, source, semantic, functions)
                    .map(HirForInitializer::Variable)
            }
            initializer => {
                lower_expression(initializer.to_expression(), source, semantic, functions)
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
                .map(|test| lower_expression(test, source, semantic, functions))
                .transpose()?,
            update: statement
                .update
                .as_ref()
                .map(|update| lower_expression(update, source, semantic, functions))
                .transpose()?,
            body: Box::new(lower_statement(
                &statement.body,
                source,
                semantic,
                functions,
                context.nested(),
            )?),
        },
    })
}

/// Owns try/catch/finally bodies while restricting the first catch slice to identifier binding.
fn lower_try_statement(
    statement: &oxc::ast::ast::TryStatement<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
    context: StatementContext,
) -> Result<HirStatement, CompileError> {
    let mut block = Vec::with_capacity(statement.block.body.len());
    for statement in &statement.block.body {
        block.push(lower_statement(
            statement,
            source,
            semantic,
            functions,
            context.nested(),
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
                    BindingPattern::BindingIdentifier(identifier) => {
                        new_binding(identifier, source, semantic)
                    }
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
                    semantic,
                    functions,
                    context.nested(),
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
                    semantic,
                    functions,
                    context.nested(),
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
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
    context: StatementContext,
) -> Result<HirStatement, CompileError> {
    let mut cases = Vec::with_capacity(statement.cases.len());
    for case in &statement.cases {
        let mut consequent = Vec::with_capacity(case.consequent.len());
        for statement in &case.consequent {
            consequent.push(lower_statement(
                statement,
                source,
                semantic,
                functions,
                context.nested(),
            )?);
        }
        cases.push(HirSwitchCase {
            span: source_span(case.span),
            test: case
                .test
                .as_ref()
                .map(|test| lower_expression(test, source, semantic, functions))
                .transpose()?,
            consequent: consequent.into(),
        });
    }
    Ok(HirStatement {
        span: source_span(statement.span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::Switch {
            discriminant: lower_expression(&statement.discriminant, source, semantic, functions)?,
            cases: cases.into(),
        },
    })
}

/// Copies one ordinary declaration and its simple parameters/body into an owned function stencil.
fn lower_function_declaration(
    function: &oxc::ast::ast::Function<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirStatement, CompileError> {
    let identifier = function.id.as_ref().ok_or_else(|| {
        unsupported(
            source.name(),
            source_span(function.span),
            "anonymous function declaration",
        )
    })?;
    let declaration_binding = new_binding(identifier, source, semantic)?;
    let id = lower_function_stencil(
        function,
        Some(Arc::from(identifier.name.as_str())),
        source,
        semantic,
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
pub(super) fn lower_function_stencil(
    function: &oxc::ast::ast::Function<'_>,
    name: Option<Arc<str>>,
    source: &SourceText,
    semantic: &Semantic<'_>,
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
    let mut parameter_initializers = Vec::with_capacity(function.params.items.len());
    for parameter in &function.params.items {
        let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
            return Err(unsupported(
                source.name(),
                source_span(parameter.pattern.span()),
                "parameter binding pattern",
            ));
        };
        parameters.push(new_binding(identifier, source, semantic)?);
        parameter_initializers.push(
            parameter
                .initializer
                .as_ref()
                .map(|initializer| lower_expression(initializer, source, semantic, functions))
                .transpose()?,
        );
    }
    let mut statements = Vec::with_capacity(body.statements.len());
    for statement in &body.statements {
        statements.push(lower_statement(
            statement,
            source,
            semantic,
            functions,
            StatementContext::FunctionBody,
        )?);
    }
    let id = FunctionStencilId(
        u32::try_from(functions.len()).map_err(|_| CompileError::BindingOverflow)?,
    );
    let oxc_scope = function
        .scope_id
        .get()
        .ok_or_else(|| missing_semantic(source, source_span(function.span), "function scope"))?;
    functions.push(HirFunction {
        id,
        span: source_span(function.span),
        name,
        parameters: parameters.into(),
        parameter_initializers: parameter_initializers.into(),
        body: statements.into(),
        scope: to_scope_id(oxc_scope),
        strict: semantic
            .scoping()
            .scope_flags(oxc_scope)
            .contains(OxcScopeFlags::StrictMode),
    });
    Ok(id)
}

/// Copies the supported synchronous arrow subset into the same owned stencil used by functions.
pub(super) fn lower_arrow_function_stencil(
    function: &oxc::ast::ast::ArrowFunctionExpression<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<FunctionStencilId, CompileError> {
    if function.r#async || function.params.rest.is_some() {
        return Err(unsupported(
            source.name(),
            source_span(function.span),
            "async/rest arrow function",
        ));
    }
    let mut parameters = Vec::with_capacity(function.params.items.len());
    let mut parameter_initializers = Vec::with_capacity(function.params.items.len());
    for parameter in &function.params.items {
        let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
            return Err(unsupported(
                source.name(),
                source_span(parameter.pattern.span()),
                "arrow parameter binding pattern",
            ));
        };
        parameters.push(new_binding(identifier, source, semantic)?);
        parameter_initializers.push(
            parameter
                .initializer
                .as_ref()
                .map(|initializer| lower_expression(initializer, source, semantic, functions))
                .transpose()?,
        );
    }
    let mut statements = Vec::with_capacity(function.body.statements.len());
    if function.expression {
        let [Statement::ExpressionStatement(statement)] = function.body.statements.as_slice()
        else {
            return Err(unsupported(
                source.name(),
                source_span(function.span),
                "arrow expression body",
            ));
        };
        statements.push(HirStatement {
            span: source_span(statement.span),
            completion: StatementCompletion::Value,
            kind: HirStatementKind::Return(Some(lower_expression(
                &statement.expression,
                source,
                semantic,
                functions,
            )?)),
        });
    } else {
        for statement in &function.body.statements {
            statements.push(lower_statement(
                statement,
                source,
                semantic,
                functions,
                StatementContext::FunctionBody,
            )?);
        }
    }
    let id = FunctionStencilId(
        u32::try_from(functions.len()).map_err(|_| CompileError::BindingOverflow)?,
    );
    let oxc_scope = function
        .scope_id
        .get()
        .ok_or_else(|| missing_semantic(source, source_span(function.span), "arrow scope"))?;
    functions.push(HirFunction {
        id,
        span: source_span(function.span),
        name: None,
        parameters: parameters.into(),
        parameter_initializers: parameter_initializers.into(),
        body: statements.into(),
        scope: to_scope_id(oxc_scope),
        strict: semantic
            .scoping()
            .scope_flags(oxc_scope)
            .contains(OxcScopeFlags::StrictMode),
    });
    Ok(id)
}

fn new_binding(
    identifier: &oxc::ast::ast::BindingIdentifier<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
) -> Result<HirBinding, CompileError> {
    let span = source_span(identifier.span);
    let symbol = identifier
        .symbol_id
        .get()
        .ok_or_else(|| missing_semantic(source, span, "binding symbol"))?;
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
        id: BindingId(symbol.index() as u32),
        scope: to_scope_id(binding_scope),
        span,
        name: Arc::from(identifier.name.as_str()),
        captured,
    })
}

/// Finds the activation-owning function scope for capture analysis without retaining Oxc IDs.
fn nearest_function_scope(
    semantic: &Semantic<'_>,
    mut scope: oxc::semantic::ScopeId,
) -> Option<oxc::semantic::ScopeId> {
    loop {
        if semantic
            .scoping()
            .scope_flags(scope)
            .contains(OxcScopeFlags::Function)
        {
            return Some(scope);
        }
        scope = semantic.scoping().scope_parent_id(scope)?;
    }
}

/// Copies simple variable declarations and assigns stable IDs before the Oxc arena is discarded.
fn lower_variable_declaration(
    declaration: &VariableDeclaration<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
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
        let binding = new_binding(identifier, source, semantic)?;
        declarators.push(HirVariableDeclarator {
            span: source_span(declarator.span),
            binding,
            initializer: declarator
                .init
                .as_ref()
                .map(|initializer| lower_expression(initializer, source, semantic, functions))
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
