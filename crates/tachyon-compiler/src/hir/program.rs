use std::sync::Arc;

use oxc::{
    ast::ast::Program,
    semantic::{ScopeFlags as OxcScopeFlags, Semantic},
};

use crate::{CompileError, ProgramKind, SourceId, SourceSpan, SourceText};

use super::expression::{HirExpression, HirExpressionKind};
use super::module::{HirModuleStencil, lower_module};
use super::pattern::HirPattern;
use super::statement::{HirStatement, HirStatementKind, StatementContext, lower_statement};
use super::{copy_string_literal, source_span, to_scope_id};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ScopeId(pub(super) u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct BindingId(pub(super) u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ReferenceId(pub(super) u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct FunctionStencilId(pub(super) u32);

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
    module_stencil: Option<HirModuleStencil>,
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

    #[must_use]
    pub const fn module_stencil(&self) -> Option<&HirModuleStencil> {
        self.module_stencil.as_ref()
    }
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
    /// The immutable lexical name visible only inside a named function expression.
    pub self_binding: Option<HirBinding>,
    pub parameters: Arc<[HirPattern]>,
    pub parameter_initializers: Arc<[Option<HirExpression>]>,
    pub rest_parameter: Option<HirPattern>,
    pub body: Arc<[HirStatement]>,
    pub scope: ScopeId,
    pub strict: bool,
    /// Whether this stencil is an arrow and therefore has no own arguments binding.
    pub is_arrow: bool,
    /// Whether a nested arrow captures this activation's implicit `arguments` binding.
    pub lexical_arguments_owner: bool,
    pub kind: HirFunctionKind,
    /// Whether this class constructor must run its attached instance-element plan.
    pub initialize_instance_elements: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirFunctionKind {
    Ordinary,
    Async,
    Generator,
    AsyncGenerator,
    DerivedClassConstructor,
    DefaultDerivedConstructor,
    BaseClassConstructor,
    DefaultBaseConstructor,
    ClassMethod,
    ClassFieldInitializer,
    ClassStaticBlock,
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

/// Lowers only the currently supported Oxc subset and copies every retained field into owned HIR.
pub(crate) fn lower(
    program: &Program<'_>,
    kind: ProgramKind,
    source: &SourceText,
    semantic: &Semantic<'_>,
) -> Result<HirProgram, CompileError> {
    let statement_capacity = program
        .directives
        .len()
        .checked_add(program.body.len())
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "program statements",
        })?;
    let mut statements = Vec::with_capacity(statement_capacity);
    let mut functions = Vec::with_capacity(program.body.len());
    for directive in &program.directives {
        statements.push(HirStatement {
            span: source_span(directive.span),
            completion: StatementCompletion::Value,
            kind: HirStatementKind::Expression(HirExpression {
                span: source_span(directive.expression.span),
                kind: HirExpressionKind::String(copy_string_literal(
                    &directive.expression,
                    source,
                )?),
            }),
        });
    }
    let module_stencil = if kind == ProgramKind::Module {
        let (stencil, module_statements) = lower_module(program, source, semantic, &mut functions)?;
        statements.extend(module_statements);
        Some(stencil)
    } else {
        for statement in &program.body {
            statements.push(lower_statement(
                statement,
                source,
                semantic,
                &mut functions,
                StatementContext::ScriptBody,
            )?);
        }
        None
    };
    let lexical_arguments_owners = lexical_arguments_owners(semantic);
    for function in &mut functions {
        function.lexical_arguments_owner = lexical_arguments_owners.contains(&function.scope);
    }
    Ok(HirProgram {
        source_id: source.id(),
        kind,
        statements: statements.into(),
        functions: functions.into(),
        root_scope: to_scope_id(semantic.scoping().root_scope_id()),
        scopes: copy_scopes(semantic).into(),
        module_stencil,
    })
}

/// Finds non-arrow function activations whose `arguments` binding escapes through an arrow.
fn lexical_arguments_owners(semantic: &Semantic<'_>) -> Vec<ScopeId> {
    let scoping = semantic.scoping();
    let Some(references) = scoping.root_unresolved_references().get("arguments") else {
        return Vec::new();
    };
    let mut owners = Vec::with_capacity(references.len());
    for reference in references.iter().copied() {
        let scope = scoping.get_reference(reference).scope_id();
        if let Some(owner) = lexical_arguments_owner(semantic, scope)
            && !owners.contains(&owner)
        {
            owners.push(owner);
        }
    }
    owners
}

/// Resolves an unresolved `arguments` reference only when its nearest function is an arrow.
pub(super) fn lexical_arguments_owner(
    semantic: &Semantic<'_>,
    mut scope: oxc::semantic::ScopeId,
) -> Option<ScopeId> {
    let scoping = semantic.scoping();
    let mut crossed_arrow = false;
    loop {
        let flags = scoping.scope_flags(scope);
        if flags.contains(OxcScopeFlags::Function) {
            if flags.contains(OxcScopeFlags::Arrow) {
                crossed_arrow = true;
            } else {
                return crossed_arrow.then(|| to_scope_id(scope));
            }
        }
        scope = scoping.scope_parent_id(scope)?;
    }
}

/// Allocates a collision-free compiler-internal binding identity for lexical `arguments`.
pub(crate) const fn lexical_arguments_binding(owner: ScopeId) -> BindingId {
    BindingId(u32::MAX - owner.index())
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
