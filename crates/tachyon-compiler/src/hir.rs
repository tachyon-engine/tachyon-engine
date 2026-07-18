//! Owned high-level IR that intentionally does not retain Oxc AST nodes or arena lifetimes.

use std::sync::Arc;

use oxc::{
    ast::ast::{
        AssignmentTarget, BindingPattern, Expression, ForStatementInit, ObjectPropertyKind,
        Program, PropertyKey, PropertyKind, SimpleAssignmentTarget, Statement, VariableDeclaration,
        VariableDeclarationKind,
    },
    semantic::{ScopeFlags as OxcScopeFlags, Semantic},
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

impl ScopeId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl BindingId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl ReferenceId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HirScopeFlags {
    pub strict: bool,
    pub function: bool,
    pub arrow: bool,
    pub direct_eval: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HirScope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub flags: HirScopeFlags,
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
    root_scope: ScopeId,
    scopes: Arc<[HirScope]>,
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

    #[must_use]
    pub const fn root_scope(&self) -> ScopeId {
        self.root_scope
    }

    #[must_use]
    pub fn scopes(&self) -> &[HirScope] {
        &self.scopes
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
    pub parameter_initializers: Arc<[Option<HirExpression>]>,
    pub body: Arc<[HirStatement]>,
    pub scope: ScopeId,
    pub strict: bool,
}

/// An owned lexical binding declaration, independent from Oxc's arena-backed identifier node.
#[derive(Clone, Debug, PartialEq)]
pub struct HirBinding {
    pub id: BindingId,
    pub scope: ScopeId,
    pub span: SourceSpan,
    pub name: Arc<str>,
    pub captured: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirIdentifierReference {
    pub id: ReferenceId,
    pub scope: ScopeId,
    pub binding: Option<BindingId>,
    pub binding_scope: Option<ScopeId>,
    pub name: Arc<str>,
    pub read: bool,
    pub write: bool,
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
pub struct HirObjectProperty {
    pub span: SourceSpan,
    pub key: HirObjectPropertyKey,
    pub value: HirExpression,
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpressionKind {
    Number(u64),
    String(Arc<str>),
    Boolean(bool),
    Null,
    Identifier(HirIdentifierReference),
    Function(FunctionStencilId),
    This,
    NewTarget,
    Object(Arc<[HirObjectProperty]>),
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
    semantic: &Semantic<'_>,
) -> Result<HirProgram, CompileError> {
    let mut statements = Vec::with_capacity(program.body.len());
    let mut functions = Vec::with_capacity(program.body.len());
    for statement in &program.body {
        statements.push(lower_statement(
            statement,
            source,
            semantic,
            &mut functions,
            StatementContext::ScriptBody,
        )?);
    }
    Ok(HirProgram {
        source_id: source.id(),
        kind,
        statements: statements.into(),
        functions: functions.into(),
        root_scope: to_scope_id(semantic.scoping().root_scope_id()),
        scopes: copy_scopes(semantic).into(),
    })
}

/// Copies Oxc's arena-owned scope tree into compact Tachyon identities and capability flags.
fn copy_scopes(semantic: &Semantic<'_>) -> Vec<HirScope> {
    let scoping = semantic.scoping();
    let mut scopes = Vec::with_capacity(scoping.scopes_len());
    for id in scoping.scope_descendants_from_root() {
        let flags = scoping.scope_flags(id);
        scopes.push(HirScope {
            id: to_scope_id(id),
            parent: scoping.scope_parent_id(id).map(to_scope_id),
            flags: HirScopeFlags {
                strict: flags.contains(OxcScopeFlags::StrictMode),
                function: flags.contains(OxcScopeFlags::Function),
                arrow: flags.contains(OxcScopeFlags::Arrow),
                direct_eval: flags.contains(OxcScopeFlags::DirectEval),
            },
        });
    }
    scopes
}

/// Copies one statement while enforcing which declaration/control forms are legal in this scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatementContext {
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

fn lower_statement(
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
fn lower_function_stencil(
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

/// Recursively copies leaf values and operands so the returned expression has no arena-backed memory.
fn lower_expression(
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
        Expression::BooleanLiteral(literal) => HirExpressionKind::Boolean(literal.value),
        Expression::NullLiteral(_) => HirExpressionKind::Null,
        Expression::Identifier(identifier) => {
            HirExpressionKind::Identifier(new_reference(identifier, source, semantic)?)
        }
        Expression::FunctionExpression(function) if function.id.is_none() => {
            HirExpressionKind::Function(lower_function_stencil(
                function, None, source, semantic, functions,
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
        Expression::ObjectExpression(expression) => {
            let mut properties = Vec::with_capacity(expression.properties.len());
            for property in &expression.properties {
                let ObjectPropertyKind::ObjectProperty(property) = property else {
                    return Err(unsupported(
                        source.name(),
                        source_span(property.span()),
                        "object spread",
                    ));
                };
                if property.kind != PropertyKind::Init {
                    return Err(unsupported(
                        source.name(),
                        source_span(property.span),
                        "object accessor",
                    ));
                }
                let key = if property.computed {
                    HirObjectPropertyKey::Computed(lower_expression(
                        property.key.to_expression(),
                        source,
                        semantic,
                        functions,
                    )?)
                } else {
                    let key = match &property.key {
                        PropertyKey::StaticIdentifier(identifier) => identifier.name.as_str(),
                        PropertyKey::StringLiteral(literal) => literal.value.as_str(),
                        _ => {
                            return Err(unsupported(
                                source.name(),
                                source_span(property.key.span()),
                                "object property key",
                            ));
                        }
                    };
                    HirObjectPropertyKey::Static(Arc::from(key))
                };
                let value = if property.method {
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
                    HirExpression {
                        span: source_span(function.span),
                        kind: HirExpressionKind::Function(lower_function_stencil(
                            function, None, source, semantic, functions,
                        )?),
                    }
                } else {
                    lower_expression(&property.value, source, semantic, functions)?
                };
                properties.push(HirObjectProperty {
                    span: source_span(property.span),
                    key,
                    value,
                });
            }
            HirExpressionKind::Object(properties.into())
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
            target: lower_assignment_target(&expression.left, source, semantic, functions)?,
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
fn new_reference(
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
