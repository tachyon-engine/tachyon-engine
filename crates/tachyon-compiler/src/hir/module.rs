use std::sync::Arc;

use oxc::{
    ast::ast::{
        Declaration, ImportAttributeKey, ImportDeclarationSpecifier, ModuleExportName, Program,
        Statement, WithClause,
    },
    ast_visit::Visit,
    semantic::Semantic,
    span::GetSpan,
    syntax::scope::ScopeFlags,
};

use crate::{CompileError, SourceText};

use super::statement::{
    StatementContext, lower_class_declaration, lower_function_declaration, lower_statement,
    lower_variable_declaration,
};
use super::{
    HirFunction, HirStatement, HirStatementKind, StatementCompletion, copy_string_literal,
    source_span, unsupported,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirModuleAttribute {
    pub key: Arc<[u16]>,
    pub value: Arc<[u16]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirModuleRequest {
    pub specifier: Arc<[u16]>,
    pub attributes: Arc<[HirModuleAttribute]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirModuleImportName {
    Name(Arc<[u16]>),
    Namespace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirModuleImportEntry {
    pub module_request: u32,
    pub import_name: HirModuleImportName,
    pub local_name: Arc<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirModuleExportEntry {
    Local {
        export_name: Arc<[u16]>,
        local_name: Arc<str>,
    },
    Indirect {
        export_name: Arc<[u16]>,
        module_request: u32,
        import_name: HirModuleImportName,
    },
    Star {
        module_request: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirModuleStencil {
    pub requested_modules: Arc<[HirModuleRequest]>,
    pub imports: Arc<[HirModuleImportEntry]>,
    pub exports: Arc<[HirModuleExportEntry]>,
    pub local_bindings: Arc<[Arc<str>]>,
    pub has_top_level_await: bool,
}

/// Extracts static module semantics before lowering executable declarations in source order.
pub(super) fn lower_module(
    program: &Program<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<(HirModuleStencil, Vec<HirStatement>), CompileError> {
    let mut builder = ModuleStencilBuilder::new(program.body.len());
    builder.collect_requests_and_imports(program, source)?;
    let mut statements = Vec::with_capacity(program.body.len());
    for statement in &program.body {
        statements.push(builder.lower_item(statement, source, semantic, functions)?);
    }
    let imported_names: Vec<_> = builder
        .imports
        .iter()
        .map(|entry| entry.local_name.clone())
        .collect();
    let root = semantic.scoping().root_scope_id();
    let mut local_bindings = semantic
        .scoping()
        .get_bindings(root)
        .keys()
        .filter(|name| {
            !imported_names
                .iter()
                .any(|imported| imported.as_ref() == name.as_str())
        })
        .map(|name| Arc::from(name.as_str()))
        .collect::<Vec<_>>();
    local_bindings.sort_unstable();
    Ok((
        HirModuleStencil {
            requested_modules: builder.requests.into(),
            imports: builder.imports.into(),
            exports: builder.exports.into(),
            local_bindings: local_bindings.into(),
            has_top_level_await: contains_top_level_await(program),
        },
        statements,
    ))
}

struct ModuleStencilBuilder {
    requests: Vec<HirModuleRequest>,
    imports: Vec<HirModuleImportEntry>,
    exports: Vec<HirModuleExportEntry>,
}

impl ModuleStencilBuilder {
    fn new(item_count: usize) -> Self {
        Self {
            requests: Vec::with_capacity(item_count),
            imports: Vec::with_capacity(item_count),
            exports: Vec::with_capacity(item_count),
        }
    }

    /// Interns requests in source order while collecting imports needed by export normalization.
    fn collect_requests_and_imports(
        &mut self,
        program: &Program<'_>,
        source: &SourceText,
    ) -> Result<(), CompileError> {
        for statement in &program.body {
            match statement {
                Statement::ImportDeclaration(declaration) => {
                    self.collect_import(declaration, source)?;
                }
                Statement::ExportAllDeclaration(declaration) => {
                    self.intern_request(
                        copy_string_literal(&declaration.source, source)?,
                        declaration.with_clause.as_deref(),
                        source,
                    )?;
                }
                Statement::ExportNamedDeclaration(declaration) => {
                    if let Some(specifier) = &declaration.source {
                        self.intern_request(
                            copy_string_literal(specifier, source)?,
                            declaration.with_clause.as_deref(),
                            source,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Records one import declaration after its source-ordered request has been interned.
    fn collect_import(
        &mut self,
        declaration: &oxc::ast::ast::ImportDeclaration<'_>,
        source: &SourceText,
    ) -> Result<(), CompileError> {
        if declaration.phase.is_some() {
            return Err(unsupported(
                source.name(),
                source_span(declaration.span),
                "source/defer import phase",
            ));
        }
        let request = self.intern_request(
            copy_string_literal(&declaration.source, source)?,
            declaration.with_clause.as_deref(),
            source,
        )?;
        for specifier in declaration.specifiers.iter().flatten() {
            let (import_name, local_name) = match specifier {
                ImportDeclarationSpecifier::ImportSpecifier(specifier) => (
                    HirModuleImportName::Name(copy_module_name(&specifier.imported, source)?),
                    Arc::from(specifier.local.name.as_str()),
                ),
                ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) => (
                    HirModuleImportName::Name(units("default")),
                    Arc::from(specifier.local.name.as_str()),
                ),
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(specifier) => (
                    HirModuleImportName::Namespace,
                    Arc::from(specifier.local.name.as_str()),
                ),
            };
            self.imports.push(HirModuleImportEntry {
                module_request: request,
                import_name,
                local_name,
            });
        }
        Ok(())
    }

    /// Converts one module item into static tables plus its executable HIR contribution.
    fn lower_item(
        &mut self,
        statement: &Statement<'_>,
        source: &SourceText,
        semantic: &Semantic<'_>,
        functions: &mut Vec<HirFunction>,
    ) -> Result<HirStatement, CompileError> {
        match statement {
            Statement::ImportDeclaration(declaration) => Ok(empty_statement(declaration.span)),
            Statement::ExportAllDeclaration(declaration) => {
                let request = self.intern_request(
                    copy_string_literal(&declaration.source, source)?,
                    declaration.with_clause.as_deref(),
                    source,
                )?;
                if let Some(exported) = &declaration.exported {
                    self.exports.push(HirModuleExportEntry::Indirect {
                        export_name: copy_module_name(exported, source)?,
                        module_request: request,
                        import_name: HirModuleImportName::Namespace,
                    });
                } else {
                    self.exports.push(HirModuleExportEntry::Star {
                        module_request: request,
                    });
                }
                Ok(empty_statement(declaration.span))
            }
            Statement::ExportNamedDeclaration(declaration) => {
                self.lower_named_export(declaration, source)?;
                declaration.declaration.as_ref().map_or_else(
                    || Ok(empty_statement(declaration.span)),
                    |declaration| {
                        lower_export_declaration(declaration, source, semantic, functions)
                    },
                )
            }
            Statement::ExportDefaultDeclaration(declaration) => Err(unsupported(
                source.name(),
                source_span(declaration.span),
                "default export synthetic binding",
            )),
            _ => lower_statement(
                statement,
                source,
                semantic,
                functions,
                StatementContext::ScriptBody,
            ),
        }
    }

    /// Partitions named exports and rewrites exports of imports into direct indirect entries.
    fn lower_named_export(
        &mut self,
        declaration: &oxc::ast::ast::ExportNamedDeclaration<'_>,
        source: &SourceText,
    ) -> Result<(), CompileError> {
        let request = declaration
            .source
            .as_ref()
            .map(|specifier| {
                self.intern_request(
                    copy_string_literal(specifier, source)?,
                    declaration.with_clause.as_deref(),
                    source,
                )
            })
            .transpose()?;
        for specifier in &declaration.specifiers {
            let export_name = copy_module_name(&specifier.exported, source)?;
            if let Some(module_request) = request {
                self.exports.push(HirModuleExportEntry::Indirect {
                    export_name,
                    module_request,
                    import_name: HirModuleImportName::Name(copy_module_name(
                        &specifier.local,
                        source,
                    )?),
                });
                continue;
            }
            let local_name = local_identifier_name(&specifier.local, source)?;
            if let Some(import) = self
                .imports
                .iter()
                .find(|entry| entry.local_name == local_name)
            {
                self.exports.push(HirModuleExportEntry::Indirect {
                    export_name,
                    module_request: import.module_request,
                    import_name: import.import_name.clone(),
                });
            } else {
                self.exports.push(HirModuleExportEntry::Local {
                    export_name,
                    local_name,
                });
            }
        }
        if let Some(declaration) = &declaration.declaration {
            for local_name in declaration_names(declaration) {
                self.exports.push(HirModuleExportEntry::Local {
                    export_name: local_name.encode_utf16().collect::<Vec<_>>().into(),
                    local_name,
                });
            }
        }
        Ok(())
    }

    fn intern_request(
        &mut self,
        specifier: Arc<[u16]>,
        clause: Option<&WithClause<'_>>,
        source: &SourceText,
    ) -> Result<u32, CompileError> {
        let attributes = copy_attributes(clause, source)?;
        let request = HirModuleRequest {
            specifier,
            attributes,
        };
        if let Some(index) = self
            .requests
            .iter()
            .position(|existing| existing == &request)
        {
            return u32::try_from(index).map_err(|_| CompileError::LoweringCapacityOverflow {
                collection: "module requests",
            });
        }
        let index = u32::try_from(self.requests.len()).map_err(|_| {
            CompileError::LoweringCapacityOverflow {
                collection: "module requests",
            }
        })?;
        self.requests.push(request);
        Ok(index)
    }
}

/// Lowers the executable declaration nested by `export` without retaining its Oxc wrapper.
fn lower_export_declaration(
    declaration: &Declaration<'_>,
    source: &SourceText,
    semantic: &Semantic<'_>,
    functions: &mut Vec<HirFunction>,
) -> Result<HirStatement, CompileError> {
    match declaration {
        Declaration::VariableDeclaration(declaration) => Ok(HirStatement {
            span: source_span(declaration.span),
            completion: StatementCompletion::Empty,
            kind: HirStatementKind::VariableDeclaration(lower_variable_declaration(
                declaration,
                source,
                semantic,
                functions,
            )?),
        }),
        Declaration::FunctionDeclaration(declaration) => {
            lower_function_declaration(declaration, source, semantic, functions)
        }
        Declaration::ClassDeclaration(declaration) => {
            lower_class_declaration(declaration, source, semantic, functions)
        }
        _ => Err(unsupported(
            source.name(),
            source_span(declaration.span()),
            "TypeScript export declaration",
        )),
    }
}

fn declaration_names(declaration: &Declaration<'_>) -> Vec<Arc<str>> {
    match declaration {
        Declaration::VariableDeclaration(declaration) => declaration
            .declarations
            .iter()
            .flat_map(|declarator| declarator.id.get_binding_identifiers())
            .map(|identifier| Arc::from(identifier.name.as_str()))
            .collect(),
        Declaration::FunctionDeclaration(declaration) => declaration
            .id
            .iter()
            .map(|identifier| Arc::from(identifier.name.as_str()))
            .collect(),
        Declaration::ClassDeclaration(declaration) => declaration
            .id
            .iter()
            .map(|identifier| Arc::from(identifier.name.as_str()))
            .collect(),
        _ => Vec::new(),
    }
}

fn copy_attributes(
    clause: Option<&WithClause<'_>>,
    source: &SourceText,
) -> Result<Arc<[HirModuleAttribute]>, CompileError> {
    let Some(clause) = clause else {
        return Ok(Arc::from([]));
    };
    let mut attributes = Vec::with_capacity(clause.with_entries.len());
    for attribute in &clause.with_entries {
        let key = match &attribute.key {
            ImportAttributeKey::Identifier(identifier) => units(identifier.name.as_str()),
            ImportAttributeKey::StringLiteral(literal) => copy_string_literal(literal, source)?,
        };
        attributes.push(HirModuleAttribute {
            key,
            value: copy_string_literal(&attribute.value, source)?,
        });
    }
    attributes.sort_unstable_by(|left, right| left.key.cmp(&right.key));
    Ok(attributes.into())
}

fn copy_module_name(
    name: &ModuleExportName<'_>,
    source: &SourceText,
) -> Result<Arc<[u16]>, CompileError> {
    match name {
        ModuleExportName::IdentifierName(identifier) => Ok(units(identifier.name.as_str())),
        ModuleExportName::IdentifierReference(identifier) => Ok(units(identifier.name.as_str())),
        ModuleExportName::StringLiteral(literal) => copy_string_literal(literal, source),
    }
}

fn local_identifier_name(
    name: &ModuleExportName<'_>,
    source: &SourceText,
) -> Result<Arc<str>, CompileError> {
    match name {
        ModuleExportName::IdentifierName(identifier) => Ok(Arc::from(identifier.name.as_str())),
        ModuleExportName::IdentifierReference(identifier) => {
            Ok(Arc::from(identifier.name.as_str()))
        }
        ModuleExportName::StringLiteral(literal) => Err(unsupported(
            source.name(),
            source_span(literal.span),
            "string local export name without from clause",
        )),
    }
}

fn units(value: &str) -> Arc<[u16]> {
    value.encode_utf16().collect::<Vec<_>>().into()
}

fn empty_statement(span: oxc::span::Span) -> HirStatement {
    HirStatement {
        span: source_span(span),
        completion: StatementCompletion::Empty,
        kind: HirStatementKind::Empty,
    }
}

fn contains_top_level_await(program: &Program<'_>) -> bool {
    let mut visitor = TopLevelAwaitVisitor::default();
    visitor.visit_program(program);
    visitor.found
}

#[derive(Default)]
struct TopLevelAwaitVisitor {
    found: bool,
}

impl<'a> Visit<'a> for TopLevelAwaitVisitor {
    fn visit_await_expression(&mut self, _expression: &oxc::ast::ast::AwaitExpression<'a>) {
        self.found = true;
    }

    fn visit_function(&mut self, _function: &oxc::ast::ast::Function<'a>, _flags: ScopeFlags) {}

    fn visit_arrow_function_expression(
        &mut self,
        _function: &oxc::ast::ast::ArrowFunctionExpression<'a>,
    ) {
    }
}
