//! Lowering of the first owned HIR subset into immutable register bytecode.

mod capacity;
mod class_environment;
mod field_initializer;
mod lowerer;

use lowerer::Lowerer;

use tachyon_bytecode::{
    BytecodeBuilder, BytecodeConstant, CompiledFunctionTemplate, CompiledModule,
    EnvironmentRecordKind, EnvironmentSlotMetadata, FunctionId, FunctionKind, FunctionLayout,
    FunctionMetadata, FunctionStrictness, HandlerEntry, Opcode,
};

use crate::hir::HirForInLeft;
use crate::{
    BindingId, CompileError, HirForInitializer, HirFunction, HirFunctionKind, HirProgram,
    HirStatement, HirStatementKind, HirVariableDeclaration, HirVariableDeclarationKind,
    ProgramKind, ScopeId, SourceSpan, SourceText,
};

/// Lowers the currently supported HIR subset while preallocating builder and constant-pool storage from HIR counts.
pub(crate) fn lower(
    source: &SourceText,
    hir: &HirProgram,
    options: crate::CompileOptions,
) -> Result<CompiledModule, CompileError> {
    let environments = EnvironmentPlans::new(hir, options.direct_eval)?;
    let module_capacity = capacity::estimate_module(hir)?;
    let mut constants = Vec::with_capacity(module_capacity.constants);
    let mut scope_names = Vec::with_capacity(module_capacity.scope_names);
    let template_capacity =
        hir.functions()
            .len()
            .checked_add(1)
            .ok_or(CompileError::LoweringCapacityOverflow {
                collection: "compiled functions",
            })?;
    let mut templates = Vec::with_capacity(template_capacity);
    templates.push(lower_entry(
        source,
        hir,
        &environments,
        &mut constants,
        &mut scope_names,
    )?);
    for (function_index, function) in hir.functions().iter().enumerate() {
        templates.push(lower_function(
            source,
            function,
            function_index,
            hir.root_scope(),
            &environments,
            &mut constants,
            &mut scope_names,
        )?);
    }
    let module = CompiledModule::new(
        source.shared_text(),
        constants,
        scope_names,
        templates,
        FunctionId::new(0),
    )
    .map_err(CompileError::Module)?;
    let Some(stencil) = hir.module_stencil() else {
        return Ok(module);
    };
    let stencil = freeze_module_stencil(stencil)?;
    module
        .with_module_stencil(stencil)
        .map_err(CompileError::Module)
}

/// Converts compiler-owned module metadata into the runtime-independent bytecode representation.
fn freeze_module_stencil(
    stencil: &crate::hir::HirModuleStencil,
) -> Result<tachyon_bytecode::ModuleStencil, CompileError> {
    use tachyon_bytecode::{
        ModuleAttribute, ModuleExportEntry, ModuleImportEntry, ModuleImportName, ModuleRequest,
        ModuleRequestId, ModuleStencil,
    };
    let import_name = |name: &crate::hir::HirModuleImportName| match name {
        crate::hir::HirModuleImportName::Name(name) => ModuleImportName::Name(name.clone()),
        crate::hir::HirModuleImportName::Namespace => ModuleImportName::Namespace,
    };
    let requests = stencil
        .requested_modules
        .iter()
        .map(|request| ModuleRequest {
            specifier: request.specifier.clone(),
            attributes: request
                .attributes
                .iter()
                .map(|attribute| ModuleAttribute {
                    key: attribute.key.clone(),
                    value: attribute.value.clone(),
                })
                .collect::<Vec<_>>()
                .into(),
        })
        .collect();
    let imports = stencil
        .imports
        .iter()
        .map(|entry| ModuleImportEntry {
            module_request: ModuleRequestId::new(entry.module_request),
            import_name: import_name(&entry.import_name),
            local_name: entry.local_name.clone(),
        })
        .collect();
    let exports = stencil
        .exports
        .iter()
        .map(|entry| match entry {
            crate::hir::HirModuleExportEntry::Local {
                export_name,
                local_name,
            } => ModuleExportEntry::Local {
                export_name: export_name.clone(),
                local_name: local_name.clone(),
            },
            crate::hir::HirModuleExportEntry::Indirect {
                export_name,
                module_request,
                import_name: name,
            } => ModuleExportEntry::Indirect {
                export_name: export_name.clone(),
                module_request: ModuleRequestId::new(*module_request),
                import_name: import_name(name),
            },
            crate::hir::HirModuleExportEntry::Star { module_request } => ModuleExportEntry::Star {
                module_request: ModuleRequestId::new(*module_request),
            },
        })
        .collect();
    ModuleStencil::new(
        requests,
        imports,
        exports,
        stencil.local_bindings.to_vec(),
        stencil.has_top_level_await,
    )
    .map_err(CompileError::ModuleStencil)
}

/// Hoists top-level function declarations, then lowers script completion into function zero.
fn lower_entry(
    source: &SourceText,
    hir: &HirProgram,
    environments: &EnvironmentPlans,
    constants: &mut Vec<BytecodeConstant>,
    scope_names: &mut Vec<std::sync::Arc<str>>,
) -> Result<CompiledFunctionTemplate, CompileError> {
    let module_scope = hir.kind() == ProgramKind::Module;
    let var_bindings = var_declared_bindings(hir.statements())?;
    let has_control_flow = hir.statements().iter().any(|statement| {
        matches!(
            statement.kind,
            HirStatementKind::Block(_)
                | HirStatementKind::Labeled { .. }
                | HirStatementKind::If { .. }
                | HirStatementKind::For { .. }
                | HirStatementKind::ForIn { .. }
                | HirStatementKind::ForOf { .. }
                | HirStatementKind::Loop { .. }
                | HirStatementKind::Switch { .. }
                | HirStatementKind::Try { .. }
                | HirStatementKind::Break
                | HirStatementKind::Continue
                | HirStatementKind::BreakLabeled(_)
                | HirStatementKind::ContinueLabeled(_)
                | HirStatementKind::Throw(_)
        )
    });
    let has_expression = hir
        .statements()
        .iter()
        .any(|statement| matches!(&statement.kind, HirStatementKind::Expression(_)));
    let entry_capacity = capacity::estimate_entry(
        hir,
        var_bindings.len(),
        environments.global_lexicals.len(),
        has_control_flow,
        has_expression,
    )?;
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(
            entry_capacity.bytecode_words,
            entry_capacity.labels,
        ),
        constants,
        scope_names,
        locals: Vec::with_capacity(entry_capacity.local_bindings),
        binding_plan: Vec::with_capacity(entry_capacity.binding_plan),
        break_targets: Vec::with_capacity(entry_capacity.break_targets),
        continue_targets: Vec::with_capacity(entry_capacity.continue_targets),
        pending_loop_labels: Vec::with_capacity(entry_capacity.loop_labels),
        handlers: Vec::with_capacity(entry_capacity.handlers),
        suspend_points: Vec::with_capacity(entry_capacity.suspend_points),
        finally_depth: 0,
        environment_depth: 0,
        next_register: 0,
        source_name: source.name().clone(),
        script_scope: true,
        module_scope,
        root_scope: hir.root_scope(),
        function_scope: Some(hir.root_scope()),
        is_arrow: false,
        is_async_generator: false,
        strict: false,
        initialize_instance_elements: false,
        proper_tail_calls: false,
        needs_argument_source: false,
        active_scope: hir.root_scope(),
        environments,
    };
    for slot in &environments.entry_slots {
        lowerer.add_environment_slot(slot)?;
    }
    for lexical in &environments.global_lexicals {
        let scope_name = lowerer.global_lexical_binding(lexical)?;
        lowerer.emit(
            Opcode::DeclareGlobalLexical,
            &[scope_name, u32::from(lexical.mutable)],
            lexical.span,
        )?;
    }
    for statement in hir.statements() {
        if let HirStatementKind::FunctionDeclaration(declaration) = &statement.kind {
            if module_scope {
                lowerer.module_function_declaration(declaration, statement.span)?;
            } else {
                lowerer.function_declaration(declaration, statement.span)?;
            }
        }
    }
    if !module_scope {
        for binding in &var_bindings {
            let scope_name = lowerer.global_binding(&binding.name, true)?;
            lowerer.emit(
                Opcode::DeclareScope,
                &[scope_name],
                SourceSpan { start: 0, end: 0 },
            )?;
        }
    }
    let result = if has_control_flow {
        let result = lowerer.load_undefined(SourceSpan { start: 0, end: 0 })?;
        for statement in hir.statements() {
            if lowerer.entry_statement(statement, result)? {
                break;
            }
        }
        result
    } else {
        match hir.statements() {
            [] => lowerer.load_undefined(SourceSpan { start: 0, end: 0 })?,
            statements => {
                let mut result = None;
                for statement in statements {
                    match &statement.kind {
                        HirStatementKind::Expression(expression) => {
                            result = Some(lowerer.expression(expression)?);
                        }
                        HirStatementKind::VariableDeclaration(declaration) => {
                            lowerer.variable_declaration(declaration)?;
                        }
                        HirStatementKind::FunctionDeclaration(_) => {}
                        HirStatementKind::Return(_) => {
                            return Err(CompileError::UnsupportedSyntax {
                                source_name: source.name().clone(),
                                span: statement.span,
                                syntax: "top-level return",
                            });
                        }
                        HirStatementKind::Block(_)
                        | HirStatementKind::Labeled { .. }
                        | HirStatementKind::If { .. }
                        | HirStatementKind::For { .. }
                        | HirStatementKind::ForIn { .. }
                        | HirStatementKind::ForOf { .. }
                        | HirStatementKind::Loop { .. }
                        | HirStatementKind::Switch { .. }
                        | HirStatementKind::Try { .. }
                        | HirStatementKind::Break
                        | HirStatementKind::Continue
                        | HirStatementKind::BreakLabeled(_)
                        | HirStatementKind::ContinueLabeled(_)
                        | HirStatementKind::Throw(_) => {
                            unreachable!("control flow uses entry lowering")
                        }
                        HirStatementKind::Empty => {}
                    }
                }
                match result {
                    Some(result) => result,
                    None => lowerer.load_undefined(SourceSpan { start: 0, end: 0 })?,
                }
            }
        }
    };
    lowerer.emit(
        Opcode::Return,
        &[result.index()],
        SourceSpan { start: 0, end: 0 },
    )?;
    debug_assert!(lowerer.suspend_points.len() <= entry_capacity.suspend_points);
    let handlers = freeze_handlers(lowerer.handlers)?;
    let suspend_points = lowerer.suspend_points.into();
    let binding_plan = lowerer.binding_plan.into();
    let (bytecode, source_map, register_count) =
        lowerer.builder.finish().map_err(CompileError::Builder)?;
    let kind = match hir.kind() {
        ProgramKind::Script => FunctionKind::Script,
        ProgramKind::Module => FunctionKind::Module,
        ProgramKind::CommonJs => FunctionKind::Script,
    };
    let metadata = FunctionMetadata {
        kind,
        role: tachyon_bytecode::FunctionRole::Ordinary,
        strictness: scope_strictness(
            source,
            hir,
            hir.root_scope(),
            SourceSpan { start: 0, end: 0 },
        )?,
        layout: FunctionLayout {
            register_count,
            environment_slot_count: u32::try_from(environments.entry_slots.len())
                .map_err(|_| CompileError::BindingOverflow)?,
            max_handler_depth: entry_capacity.max_handler_depth,
            max_completion_depth: entry_capacity.max_completion_depth,
            ..FunctionLayout::default()
        },
        source_map,
        handlers,
        suspend_points,
        feedback_sites: Default::default(),
        binding_plan,
        environment_record_kind: if module_scope {
            EnvironmentRecordKind::Module
        } else if environments.entry_slots.is_empty() {
            EnvironmentRecordKind::for_function_kind(kind)
        } else {
            EnvironmentRecordKind::Declarative
        },
        environment_slots: freeze_environment_slot_metadata(&environments.entry_slots),
    };
    Ok(CompiledFunctionTemplate::new(
        FunctionId::new(0),
        bytecode,
        metadata,
    ))
}

#[derive(Clone, Debug)]
struct CapturedSlot {
    id: BindingId,
    slot: u32,
    name: std::sync::Arc<str>,
    mutable: bool,
    initialized: bool,
    parameter: bool,
}

#[derive(Clone, Debug)]
struct FunctionEnvironmentPlan {
    scope: ScopeId,
    slots: Vec<CapturedSlot>,
}

#[derive(Clone, Debug)]
struct ClassEnvironmentPlan {
    scope: ScopeId,
    name: Option<CapturedSlot>,
    private_names: Box<[PrivateNameSlot]>,
}

#[derive(Clone, Debug)]
struct IterationEnvironmentPlan {
    scope: ScopeId,
    slots: Vec<CapturedSlot>,
}

#[derive(Clone, Debug)]
struct PrivateNameSlot {
    id: crate::hir::HirPrivateNameId,
    name: std::sync::Arc<str>,
    slot: u32,
}

#[derive(Clone, Debug)]
struct EnvironmentPlans {
    root_scope: ScopeId,
    entry_slots: Vec<CapturedSlot>,
    functions: Vec<FunctionEnvironmentPlan>,
    classes: Vec<ClassEnvironmentPlan>,
    iterations: Vec<IterationEnvironmentPlan>,
    global_lexicals: Vec<GlobalLexicalPlan>,
    function_self_bindings: Vec<BindingId>,
    scopes: std::sync::Arc<[crate::HirScope]>,
}

#[derive(Clone, Debug)]
struct GlobalLexicalPlan {
    id: BindingId,
    name: std::sync::Arc<str>,
    mutable: bool,
    span: SourceSpan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnvironmentStorageKind {
    Activation,
    Class,
    Lexical,
}

impl EnvironmentPlans {
    /// Assigns slots to captures and dynamic-name bindings without reclassifying root globals.
    fn new(hir: &HirProgram, direct_eval: bool) -> Result<Self, CompileError> {
        let module_entry = hir.kind() == ProgramKind::Module;
        let mut global_lexicals =
            Vec::with_capacity(capacity::estimate_var_bindings(hir.statements())?);
        let mut entry_slots = if module_entry {
            module_entry_slots(hir)?
        } else {
            Vec::with_capacity(capacity::estimate_var_bindings(hir.statements())?)
        };
        if !module_entry {
            for statement in hir.statements() {
                let HirStatementKind::VariableDeclaration(declaration) = &statement.kind else {
                    continue;
                };
                if !matches!(
                    declaration.kind,
                    HirVariableDeclarationKind::Let | HirVariableDeclarationKind::Const
                ) {
                    continue;
                }
                for declarator in declaration.declarators.iter() {
                    let bindings = pattern_bindings(&declarator.pattern);
                    for binding in bindings {
                        if binding.scope == hir.root_scope() {
                            if direct_eval {
                                let slot = u32::try_from(entry_slots.len())
                                    .map_err(|_| CompileError::BindingOverflow)?;
                                entry_slots.push(CapturedSlot {
                                    id: binding.id,
                                    slot,
                                    name: binding.name.clone(),
                                    mutable: declaration.kind == HirVariableDeclarationKind::Let,
                                    initialized: false,
                                    parameter: false,
                                });
                            } else {
                                global_lexicals.push(GlobalLexicalPlan {
                                    id: binding.id,
                                    name: binding.name.clone(),
                                    mutable: declaration.kind == HirVariableDeclarationKind::Let,
                                    span: declarator.span,
                                });
                            }
                        }
                    }
                }
            }
        }
        let class_environments = class_environment::collect(hir);
        let mut iterations = Vec::new();
        collect_iteration_environments(hir.statements(), &mut iterations)?;
        for function in hir.functions() {
            collect_iteration_environments(&function.body, &mut iterations)?;
        }
        let iteration_bindings = iterations
            .iter()
            .flat_map(|environment| environment.slots.iter().map(|slot| slot.id))
            .collect::<Vec<_>>();
        let mut forced_captures = field_initializer::forced_captures(hir);
        forced_captures.retain(|binding| {
            !class_environments.iter().any(|class| {
                class
                    .name_binding
                    .as_ref()
                    .is_some_and(|name| name.id == *binding)
            })
        });
        let force_entry_dynamic_bindings = hir
            .scopes()
            .get(hir.root_scope().index() as usize)
            .is_some_and(|scope| scope.flags.direct_eval);
        collect_captured_slots(
            hir.statements(),
            force_entry_dynamic_bindings,
            &forced_captures,
            Some(hir.root_scope()),
            &mut entry_slots,
        )?;
        entry_slots.retain(|slot| !iteration_bindings.contains(&slot.id));
        let mut functions = Vec::with_capacity(hir.functions().len());
        let mut function_self_bindings = Vec::with_capacity(hir.functions().len());
        for function in hir.functions() {
            let force_dynamic_bindings = hir
                .scopes()
                .get(function.scope.index() as usize)
                .is_some_and(|scope| scope.flags.direct_eval);
            let parameter_binding_count = function
                .parameters
                .iter()
                .try_fold(0_usize, |count, parameter| {
                    count.checked_add(pattern_bindings(parameter).len())
                })
                .and_then(|count| {
                    function
                        .rest_parameter
                        .as_ref()
                        .map_or(Some(count), |rest| {
                            count.checked_add(pattern_bindings(rest).len())
                        })
                })
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "captured environment slots",
                })?;
            let capacity = parameter_binding_count
                .checked_add(capacity::estimate_var_bindings(&function.body)?)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "captured environment slots",
                })?;
            let mut slots = Vec::with_capacity(capacity);
            let needs_parameter_environment =
                function.parameter_initializers.iter().any(Option::is_some);
            if let Some(binding) = &function.self_binding {
                push_function_name_slot(binding, &mut slots)?;
                function_self_bindings.push(binding.id);
            }
            if function.lexical_arguments_owner {
                let slot = u32::try_from(slots.len()).map_err(|_| CompileError::BindingOverflow)?;
                slots.push(CapturedSlot {
                    id: crate::hir::lexical_arguments_binding(function.scope),
                    slot,
                    name: std::sync::Arc::from("arguments"),
                    mutable: false,
                    initialized: false,
                    parameter: false,
                });
            }
            for parameter in function.parameters.iter() {
                for binding in pattern_bindings(parameter) {
                    push_captured_slot(
                        &binding,
                        true,
                        !needs_parameter_environment,
                        // Parameter bindings must exist in the parameter environment even when
                        // no nested closure captures them: default initializers observe TDZ.
                        force_dynamic_bindings || needs_parameter_environment,
                        &forced_captures,
                        &mut slots,
                    )?;
                    if needs_parameter_environment
                        && let Some(slot) = slots.iter_mut().find(|slot| slot.id == binding.id)
                    {
                        slot.parameter = true;
                    }
                }
            }
            if let Some(rest) = &function.rest_parameter {
                for binding in pattern_bindings(rest) {
                    push_captured_slot(
                        &binding,
                        true,
                        true,
                        force_dynamic_bindings,
                        &forced_captures,
                        &mut slots,
                    )?;
                }
            }
            collect_captured_slots(
                &function.body,
                force_dynamic_bindings,
                &forced_captures,
                None,
                &mut slots,
            )?;
            slots.retain(|slot| !iteration_bindings.contains(&slot.id));
            functions.push(FunctionEnvironmentPlan {
                scope: function.scope,
                slots,
            });
        }
        let classes = class_environments
            .into_iter()
            .map(|class| {
                let private_base = u32::from(class.name_binding.is_some());
                let private_names = class
                    .private_names
                    .iter()
                    .enumerate()
                    .map(|(index, private)| {
                        Ok(PrivateNameSlot {
                            id: private.id,
                            name: private.name.clone(),
                            slot: private_base
                                .checked_add(
                                    u32::try_from(index)
                                        .map_err(|_| CompileError::BindingOverflow)?,
                                )
                                .ok_or(CompileError::BindingOverflow)?,
                        })
                    })
                    .collect::<Result<Box<[_]>, CompileError>>()?;
                Ok(ClassEnvironmentPlan {
                    scope: class.scope,
                    name: class.name_binding.map(|binding| CapturedSlot {
                        id: binding.id,
                        slot: 0,
                        name: binding.name,
                        mutable: false,
                        initialized: false,
                        parameter: false,
                    }),
                    private_names,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        Ok(Self {
            root_scope: hir.root_scope(),
            entry_slots,
            functions,
            classes,
            iterations,
            global_lexicals,
            function_self_bindings,
            scopes: hir.scopes().into(),
        })
    }

    #[inline(always)]
    fn local_slot(&self, function_scope: ScopeId, binding: BindingId) -> Option<&CapturedSlot> {
        if function_scope == self.scopes_root() {
            return self.entry_slots.iter().find(|slot| slot.id == binding);
        }
        self.functions
            .iter()
            .find(|function| function.scope == function_scope)?
            .slots
            .iter()
            .find(|slot| slot.id == binding)
    }

    #[inline(always)]
    fn global_lexical(&self, binding: BindingId) -> Option<&GlobalLexicalPlan> {
        self.global_lexicals
            .iter()
            .find(|lexical| lexical.id == binding)
    }

    #[inline(always)]
    fn iteration_environment(&self, scope: ScopeId) -> Option<&IterationEnvironmentPlan> {
        self.iterations
            .iter()
            .find(|environment| environment.scope == scope)
    }

    #[inline(always)]
    fn iteration_slot(&self, scope: ScopeId, binding: BindingId) -> Option<&CapturedSlot> {
        self.iteration_environment(scope)?
            .slots
            .iter()
            .find(|slot| slot.id == binding)
    }

    #[inline(always)]
    fn scopes_root(&self) -> ScopeId {
        self.root_scope
    }

    /// Identifies immutable named-function expression bindings through nested closure scopes.
    #[inline(always)]
    fn is_function_self_binding(&self, binding: BindingId) -> bool {
        self.function_self_bindings.contains(&binding)
    }

    /// Resolves a captured reference to the nearest allocated environment chain node.
    fn reference_slot(
        &self,
        current_scope: ScopeId,
        binding: BindingId,
    ) -> Option<(u32, &CapturedSlot, EnvironmentStorageKind)> {
        let mut cursor = if self.scope_has_environment(current_scope) {
            current_scope
        } else {
            self.nearest_environment_parent(current_scope)?
        };
        let mut depth = 0_u32;
        loop {
            if let Some(class) = self.classes.iter().find(|class| {
                class.scope == cursor && class.name.as_ref().is_some_and(|name| name.id == binding)
            }) {
                return Some((
                    depth,
                    class.name.as_ref().expect("matched class-name binding"),
                    EnvironmentStorageKind::Class,
                ));
            }
            if let Some(slot) = self
                .iterations
                .iter()
                .find(|environment| environment.scope == cursor)
                .and_then(|environment| environment.slots.iter().find(|slot| slot.id == binding))
            {
                return Some((depth, slot, EnvironmentStorageKind::Lexical));
            }
            if let Some(slot) = self
                .functions
                .iter()
                .find(|function| function.scope == cursor)
                .and_then(|function| function.slots.iter().find(|slot| slot.id == binding))
            {
                return Some((depth, slot, EnvironmentStorageKind::Activation));
            }
            if let Some(slot) = self
                .entry_slots
                .iter()
                .find(|slot| slot.id == binding && cursor == self.scopes_root())
            {
                return Some((depth, slot, EnvironmentStorageKind::Activation));
            }
            cursor = self.nearest_environment_parent(cursor)?;
            depth = depth.checked_add(1)?;
        }
    }

    /// Resolves a private name to the nearest active class environment slot.
    fn private_reference_slot(
        &self,
        current_scope: ScopeId,
        private_name: crate::hir::HirPrivateNameId,
    ) -> Option<(u32, &PrivateNameSlot)> {
        let mut cursor = if self.scope_has_environment(current_scope) {
            current_scope
        } else {
            self.nearest_environment_parent(current_scope)?
        };
        let mut depth = 0_u32;
        loop {
            if let Some(slot) = self
                .classes
                .iter()
                .find(|class| class.scope == cursor)
                .and_then(|class| {
                    class
                        .private_names
                        .iter()
                        .find(|slot| slot.id == private_name)
                })
            {
                return Some((depth, slot));
            }
            cursor = self.nearest_environment_parent(cursor)?;
            depth = depth.checked_add(1)?;
        }
    }

    fn scope_has_environment(&self, scope: ScopeId) -> bool {
        (scope == self.scopes_root() && !self.entry_slots.is_empty())
            || self
                .functions
                .iter()
                .any(|function| function.scope == scope && !function.slots.is_empty())
            || self
                .iterations
                .iter()
                .any(|environment| environment.scope == scope && !environment.slots.is_empty())
            || self.classes.iter().any(|class| class.scope == scope)
    }

    fn nearest_environment_parent(&self, scope: ScopeId) -> Option<ScopeId> {
        let mut parent = self.scopes.get(scope.index() as usize)?.parent;
        while let Some(scope) = parent {
            if self.scope_has_environment(scope) {
                return Some(scope);
            }
            parent = self.scopes.get(scope.index() as usize)?.parent;
        }
        None
    }
}

/// Builds the compiler/runtime shared module-slot order: imports first, then sorted local names.
fn module_entry_slots(hir: &HirProgram) -> Result<Vec<CapturedSlot>, CompileError> {
    let stencil = hir.module_stencil().ok_or(CompileError::BindingOverflow)?;
    let capacity = stencil
        .imports
        .len()
        .checked_add(stencil.local_bindings.len())
        .ok_or(CompileError::BindingOverflow)?;
    let mut slots = Vec::with_capacity(capacity);
    for import in stencil.imports.iter() {
        let slot = u32::try_from(slots.len()).map_err(|_| CompileError::BindingOverflow)?;
        slots.push(CapturedSlot {
            id: import.binding,
            slot,
            name: import.local_name.clone(),
            mutable: false,
            initialized: true,
            parameter: false,
        });
    }
    let var_bindings = var_declared_bindings(hir.statements())?;
    for name in stencil.local_bindings.iter() {
        let (binding, mutable, initialized) =
            module_local_binding(hir, &var_bindings, name).ok_or(CompileError::BindingOverflow)?;
        let slot = u32::try_from(slots.len()).map_err(|_| CompileError::BindingOverflow)?;
        slots.push(CapturedSlot {
            id: binding.id,
            slot,
            name: name.clone(),
            mutable,
            initialized,
            parameter: false,
        });
    }
    Ok(slots)
}

/// Recovers declaration state for one normalized local module binding.
fn module_local_binding(
    hir: &HirProgram,
    var_bindings: &[crate::HirBinding],
    name: &str,
) -> Option<(crate::HirBinding, bool, bool)> {
    if let Some(binding) = var_bindings
        .iter()
        .find(|binding| binding.name.as_ref() == name)
    {
        return Some((binding.clone(), true, true));
    }
    for statement in hir.statements() {
        match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => {
                for declarator in declaration.declarators.iter() {
                    if let Some(binding) = pattern_bindings(&declarator.pattern)
                        .into_iter()
                        .find(|binding| binding.name.as_ref() == name)
                    {
                        return Some((
                            binding,
                            declaration.kind != HirVariableDeclarationKind::Const,
                            false,
                        ));
                    }
                }
            }
            HirStatementKind::FunctionDeclaration(declaration)
                if declaration.binding.name.as_ref() == name =>
            {
                return Some((declaration.binding.clone(), true, false));
            }
            _ => {}
        }
    }
    None
}

fn push_captured_slot(
    binding: &crate::HirBinding,
    mutable: bool,
    initialized: bool,
    force_dynamic_binding: bool,
    forced_captures: &[BindingId],
    slots: &mut Vec<CapturedSlot>,
) -> Result<(), CompileError> {
    if (!force_dynamic_binding && !binding.captured && !forced_captures.contains(&binding.id))
        || slots.iter().any(|slot| slot.id == binding.id)
    {
        return Ok(());
    }
    let slot = u32::try_from(slots.len()).map_err(|_| CompileError::BindingOverflow)?;
    slots.push(CapturedSlot {
        id: binding.id,
        slot,
        name: binding.name.clone(),
        mutable,
        initialized,
        parameter: false,
    });
    Ok(())
}

/// Reserves the lexical self binding for a named function expression even without a nested capture.
fn push_function_name_slot(
    binding: &crate::HirBinding,
    slots: &mut Vec<CapturedSlot>,
) -> Result<(), CompileError> {
    if slots.iter().any(|slot| slot.id == binding.id) {
        return Ok(());
    }
    let slot = u32::try_from(slots.len()).map_err(|_| CompileError::BindingOverflow)?;
    slots.push(CapturedSlot {
        id: binding.id,
        slot,
        name: binding.name.clone(),
        mutable: false,
        initialized: true,
        parameter: false,
    });
    Ok(())
}

/// Collects every declaration leaf from an owned recursive pattern.
fn pattern_bindings(pattern: &crate::HirPattern) -> Vec<crate::HirBinding> {
    let mut bindings = Vec::new();
    collect_pattern_bindings(pattern, &mut bindings);
    bindings
}

fn collect_pattern_bindings(pattern: &crate::HirPattern, bindings: &mut Vec<crate::HirBinding>) {
    match &pattern.kind {
        crate::HirPatternKind::Binding(binding) => bindings.push(binding.clone()),
        crate::HirPatternKind::Default { target, .. } => collect_pattern_bindings(target, bindings),
        crate::HirPatternKind::Array { elements, rest } => {
            for element in elements.iter().flatten() {
                collect_pattern_bindings(element, bindings);
            }
            if let Some(rest) = rest {
                collect_pattern_bindings(rest, bindings);
            }
        }
        crate::HirPatternKind::Object { properties, rest } => {
            for property in properties.iter() {
                collect_pattern_bindings(&property.target, bindings);
            }
            if let Some(rest) = rest {
                collect_pattern_bindings(rest, bindings);
            }
        }
        crate::HirPatternKind::Assignment(_) => {}
    }
}

/// Collects captured lexical for-in/of heads into independently allocated iteration records.
fn collect_iteration_environments(
    statements: &[HirStatement],
    environments: &mut Vec<IterationEnvironmentPlan>,
) -> Result<(), CompileError> {
    for statement in statements {
        match &statement.kind {
            HirStatementKind::ForIn { left, body, .. }
            | HirStatementKind::ForOf { left, body, .. } => {
                if let HirForInLeft::Variable(declaration) = left
                    && declaration.kind != HirVariableDeclarationKind::Var
                {
                    let mutable = declaration.kind == HirVariableDeclarationKind::Let;
                    let declarator = declaration
                        .declarators
                        .first()
                        .expect("HIR validates one loop-head declarator");
                    let bindings = pattern_bindings(&declarator.pattern)
                        .into_iter()
                        .filter(|binding| binding.captured)
                        .collect::<Vec<_>>();
                    if let Some(scope) = bindings.first().map(|binding| binding.scope) {
                        let mut slots = Vec::with_capacity(bindings.len());
                        for binding in bindings {
                            let slot = u32::try_from(slots.len())
                                .map_err(|_| CompileError::BindingOverflow)?;
                            slots.push(CapturedSlot {
                                id: binding.id,
                                slot,
                                name: binding.name,
                                mutable,
                                initialized: false,
                                parameter: false,
                            });
                        }
                        environments.push(IterationEnvironmentPlan { scope, slots });
                    }
                }
                collect_iteration_environments(core::slice::from_ref(body), environments)?;
            }
            HirStatementKind::Block(body) => {
                collect_iteration_environments(body, environments)?;
            }
            HirStatementKind::Labeled { body, .. } | HirStatementKind::Loop { body, .. } => {
                collect_iteration_environments(core::slice::from_ref(body), environments)?;
            }
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                collect_iteration_environments(core::slice::from_ref(consequent), environments)?;
                if let Some(alternate) = alternate {
                    collect_iteration_environments(core::slice::from_ref(alternate), environments)?;
                }
            }
            HirStatementKind::For { body, .. } => {
                collect_iteration_environments(core::slice::from_ref(body), environments)?;
            }
            HirStatementKind::Switch { cases, .. } => {
                for case in cases.iter() {
                    collect_iteration_environments(&case.consequent, environments)?;
                }
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                collect_iteration_environments(block, environments)?;
                if let Some(handler) = handler {
                    collect_iteration_environments(&handler.body, environments)?;
                }
                if let Some(finalizer) = finalizer {
                    collect_iteration_environments(finalizer, environments)?;
                }
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::BreakLabeled(_)
            | HirStatementKind::ContinueLabeled(_)
            | HirStatementKind::Empty => {}
        }
    }
    Ok(())
}

/// Walks activation-owned declarations while nested function bodies remain separate stencils.
fn collect_captured_slots(
    statements: &[HirStatement],
    force_dynamic_bindings: bool,
    forced_captures: &[BindingId],
    excluded_scope: Option<ScopeId>,
    slots: &mut Vec<CapturedSlot>,
) -> Result<(), CompileError> {
    for statement in statements {
        match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => {
                let mutable = declaration.kind != HirVariableDeclarationKind::Const;
                let initialized = declaration.kind == HirVariableDeclarationKind::Var;
                for declarator in declaration.declarators.iter() {
                    for binding in pattern_bindings(&declarator.pattern) {
                        if excluded_scope == Some(binding.scope) {
                            continue;
                        }
                        push_captured_slot(
                            &binding,
                            mutable,
                            initialized,
                            force_dynamic_bindings,
                            forced_captures,
                            slots,
                        )?;
                    }
                }
            }
            HirStatementKind::FunctionDeclaration(declaration) => {
                if excluded_scope == Some(declaration.binding.scope) {
                    continue;
                }
                push_captured_slot(
                    &declaration.binding,
                    true,
                    true,
                    force_dynamic_bindings,
                    forced_captures,
                    slots,
                )?;
            }
            HirStatementKind::Block(body) => collect_captured_slots(
                body,
                force_dynamic_bindings,
                forced_captures,
                excluded_scope,
                slots,
            )?,
            HirStatementKind::Labeled { body, .. } => collect_captured_slots(
                core::slice::from_ref(body),
                force_dynamic_bindings,
                forced_captures,
                excluded_scope,
                slots,
            )?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                collect_captured_slots(
                    core::slice::from_ref(consequent),
                    force_dynamic_bindings,
                    forced_captures,
                    excluded_scope,
                    slots,
                )?;
                if let Some(alternate) = alternate {
                    collect_captured_slots(
                        core::slice::from_ref(alternate),
                        force_dynamic_bindings,
                        forced_captures,
                        excluded_scope,
                        slots,
                    )?;
                }
            }
            HirStatementKind::For {
                initializer, body, ..
            } => {
                if let Some(HirForInitializer::Variable(declaration)) = initializer {
                    let mutable = declaration.kind != HirVariableDeclarationKind::Const;
                    let initialized = declaration.kind == HirVariableDeclarationKind::Var;
                    for declarator in declaration.declarators.iter() {
                        for binding in pattern_bindings(&declarator.pattern) {
                            if excluded_scope == Some(binding.scope) {
                                continue;
                            }
                            push_captured_slot(
                                &binding,
                                mutable,
                                initialized,
                                force_dynamic_bindings,
                                forced_captures,
                                slots,
                            )?;
                        }
                    }
                }
                collect_captured_slots(
                    core::slice::from_ref(body),
                    force_dynamic_bindings,
                    forced_captures,
                    excluded_scope,
                    slots,
                )?;
            }
            HirStatementKind::ForIn { left, body, .. } => {
                if let HirForInLeft::Variable(declaration) = left {
                    let mutable = declaration.kind != HirVariableDeclarationKind::Const;
                    let initialized = declaration.kind == HirVariableDeclarationKind::Var;
                    for declarator in declaration.declarators.iter() {
                        for binding in pattern_bindings(&declarator.pattern) {
                            if excluded_scope == Some(binding.scope) {
                                continue;
                            }
                            push_captured_slot(
                                &binding,
                                mutable,
                                initialized,
                                force_dynamic_bindings,
                                forced_captures,
                                slots,
                            )?;
                        }
                    }
                }
                collect_captured_slots(
                    core::slice::from_ref(body),
                    force_dynamic_bindings,
                    forced_captures,
                    excluded_scope,
                    slots,
                )?;
            }
            HirStatementKind::ForOf { left, body, .. } => {
                if let HirForInLeft::Variable(declaration) = left {
                    let mutable = declaration.kind != HirVariableDeclarationKind::Const;
                    let initialized = declaration.kind == HirVariableDeclarationKind::Var;
                    for declarator in declaration.declarators.iter() {
                        for binding in pattern_bindings(&declarator.pattern) {
                            if excluded_scope == Some(binding.scope) {
                                continue;
                            }
                            push_captured_slot(
                                &binding,
                                mutable,
                                initialized,
                                force_dynamic_bindings,
                                forced_captures,
                                slots,
                            )?;
                        }
                    }
                }
                collect_captured_slots(
                    core::slice::from_ref(body),
                    force_dynamic_bindings,
                    forced_captures,
                    excluded_scope,
                    slots,
                )?;
            }
            HirStatementKind::Loop { body, .. } => {
                collect_captured_slots(
                    core::slice::from_ref(body),
                    force_dynamic_bindings,
                    forced_captures,
                    excluded_scope,
                    slots,
                )?;
            }
            HirStatementKind::Switch { cases, .. } => {
                for case in cases.iter() {
                    collect_captured_slots(
                        &case.consequent,
                        force_dynamic_bindings,
                        forced_captures,
                        excluded_scope,
                        slots,
                    )?;
                }
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                collect_captured_slots(
                    block,
                    force_dynamic_bindings,
                    forced_captures,
                    excluded_scope,
                    slots,
                )?;
                if let Some(handler) = handler {
                    if let Some(parameter) = &handler.parameter {
                        for binding in pattern_bindings(parameter) {
                            if excluded_scope == Some(binding.scope) {
                                continue;
                            }
                            push_captured_slot(
                                &binding,
                                true,
                                false,
                                force_dynamic_bindings,
                                forced_captures,
                                slots,
                            )?;
                        }
                    }
                    collect_captured_slots(
                        &handler.body,
                        force_dynamic_bindings,
                        forced_captures,
                        excluded_scope,
                        slots,
                    )?;
                }
                if let Some(finalizer) = finalizer {
                    collect_captured_slots(
                        finalizer,
                        force_dynamic_bindings,
                        forced_captures,
                        excluded_scope,
                        slots,
                    )?;
                }
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::BreakLabeled(_)
            | HirStatementKind::ContinueLabeled(_)
            | HirStatementKind::Empty => {}
        }
    }
    Ok(())
}

/// Lowers one ordinary function with parameter registers fixed at the front of its frame.
fn lower_function(
    source: &SourceText,
    function: &HirFunction,
    function_index: usize,
    root_scope: ScopeId,
    environments: &EnvironmentPlans,
    constants: &mut Vec<BytecodeConstant>,
    scope_names: &mut Vec<std::sync::Arc<str>>,
) -> Result<CompiledFunctionTemplate, CompileError> {
    let var_bindings = var_declared_bindings(&function.body)?;
    let var_initialization_count = var_bindings
        .iter()
        .filter(|binding| {
            !function.parameters.iter().any(|parameter| {
                parameter
                    .binding()
                    .is_some_and(|parameter| parameter.id == binding.id)
            })
        })
        .count();
    let captured_slots = &environments.functions[function_index].slots;
    let function_capacity = capacity::estimate_function(function, var_initialization_count)?;
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(
            function_capacity.bytecode_words,
            function_capacity.labels,
        ),
        constants,
        scope_names,
        locals: Vec::with_capacity(function_capacity.local_bindings),
        binding_plan: Vec::with_capacity(function_capacity.binding_plan),
        break_targets: Vec::with_capacity(function_capacity.break_targets),
        continue_targets: Vec::with_capacity(function_capacity.continue_targets),
        pending_loop_labels: Vec::with_capacity(function_capacity.loop_labels),
        handlers: Vec::with_capacity(function_capacity.handlers),
        suspend_points: Vec::with_capacity(function_capacity.suspend_points),
        finally_depth: 0,
        environment_depth: 0,
        next_register: 0,
        source_name: source.name().clone(),
        script_scope: false,
        module_scope: false,
        root_scope,
        function_scope: Some(function.scope),
        is_arrow: function.is_arrow,
        is_async_generator: function.kind == HirFunctionKind::AsyncGenerator,
        strict: function.strict,
        initialize_instance_elements: function.initialize_instance_elements,
        proper_tail_calls: function.strict
            && function.kind == HirFunctionKind::Ordinary
            && function.role != crate::HirFunctionRole::ClassFieldInitializer,
        needs_argument_source: function.rest_parameter.is_some(),
        active_scope: function.scope,
        environments,
    };
    let synthetic_terminal = if function.kind == HirFunctionKind::DefaultDerivedConstructor {
        debug_assert!(function.parameters.is_empty());
        debug_assert!(function.parameter_initializers.is_empty());
        debug_assert!(function.rest_parameter.is_none());
        debug_assert!(function.body.is_empty());
        let result = lowerer.register()?;
        lowerer.emit(
            Opcode::SuperConstructForwardAll,
            &[result.index()],
            function.span,
        )?;
        lowerer.emit(Opcode::InitializeThis, &[result.index()], function.span)?;
        if function.initialize_instance_elements {
            lowerer.emit(
                Opcode::InitializeInstanceElements,
                &[result.index()],
                function.span,
            )?;
        }
        lowerer.emit(Opcode::ReturnUndefined, &[], function.span)?;
        true
    } else if function.kind == HirFunctionKind::DefaultBaseConstructor {
        debug_assert!(function.parameters.is_empty());
        debug_assert!(function.parameter_initializers.is_empty());
        debug_assert!(function.rest_parameter.is_none());
        debug_assert!(function.body.is_empty());
        if function.initialize_instance_elements {
            let scratch = lowerer.register()?;
            lowerer.emit(
                Opcode::InitializeInstanceElements,
                &[scratch.index()],
                function.span,
            )?;
        }
        lowerer.emit(Opcode::ReturnUndefined, &[], function.span)?;
        true
    } else {
        false
    };
    if let Some(binding) = &function.self_binding {
        lowerer.add_local(binding, None, false)?;
    }
    let mut parameter_registers = Vec::with_capacity(function.parameters.len());
    for _ in function.parameters.iter() {
        parameter_registers.push(lowerer.register()?);
    }
    if function.initialize_instance_elements
        && function.kind == HirFunctionKind::BaseClassConstructor
    {
        let scratch = lowerer.register()?;
        lowerer.emit(
            Opcode::InitializeInstanceElements,
            &[scratch.index()],
            function.span,
        )?;
    }
    if function.parameter_initializers.iter().any(Option::is_some) {
        // Install every parameter name before evaluating any initializer. This gives `x = x`
        // and references to later parameters a real TDZ environment binding instead of treating
        // the name as an unresolved global.
        for parameter in function.parameters.iter() {
            for binding in pattern_bindings(parameter) {
                lowerer.add_local(&binding, None, true)?;
            }
        }
    }
    for (index, (parameter, register)) in function
        .parameters
        .iter()
        .zip(parameter_registers)
        .enumerate()
    {
        // Keep the current parameter uninitialized while its default expression runs. Earlier
        // parameters are already published, while later/self references correctly hit TDZ.
        if let Some(initializer) = function.parameter_initializers[index].as_ref() {
            lowerer.parameter_initializer(register, initializer)?;
        }
        lowerer.bind_pattern(parameter, register, true)?;
    }
    if let Some(rest) = &function.rest_parameter {
        let rest_value = lowerer.register()?;
        let start =
            u32::try_from(function.parameters.len()).map_err(|_| CompileError::RegisterOverflow)?;
        lowerer.emit(
            Opcode::CollectRestArguments,
            &[rest_value.index(), start],
            rest.span,
        )?;
        lowerer.bind_pattern(rest, rest_value, true)?;
    }
    if function.lexical_arguments_owner {
        let arguments = lowerer.register()?;
        lowerer.needs_argument_source = true;
        lowerer.emit(
            Opcode::LoadArgumentsObject,
            &[arguments.index()],
            function.span,
        )?;
        let slot = environments
            .local_slot(
                function.scope,
                crate::hir::lexical_arguments_binding(function.scope),
            )
            .ok_or(CompileError::BindingOverflow)?;
        lowerer.emit(
            Opcode::InitializeEnvironment,
            &[arguments.index(), 0, slot.slot],
            function.span,
        )?;
    }
    for binding in &var_bindings {
        if lowerer.local_by_id(binding.id).is_some() {
            continue;
        }
        if environments
            .local_slot(function.scope, binding.id)
            .is_some()
        {
            lowerer.add_local(binding, None, true)?;
        } else {
            let register = lowerer.load_undefined(function.span)?;
            lowerer.add_local(binding, Some(register), true)?;
        }
    }
    for statement in function.body.iter() {
        if let HirStatementKind::FunctionDeclaration(declaration) = &statement.kind {
            lowerer.local_function_declaration(declaration, statement.span)?;
        }
    }
    if matches!(
        function.kind,
        HirFunctionKind::Generator | HirFunctionKind::AsyncGenerator
    ) {
        lowerer.emit(Opcode::InitialYield, &[], function.span)?;
    }
    let mut terminal = synthetic_terminal;
    for statement in function.body.iter() {
        if matches!(statement.kind, HirStatementKind::FunctionDeclaration(_)) {
            continue;
        }
        terminal = lowerer.function_statement(statement)?;
        if terminal {
            break;
        }
    }
    if !terminal {
        lowerer.emit(Opcode::ReturnUndefined, &[], function.span)?;
    }
    let name_scope = function
        .name
        .as_ref()
        .map(|name| lowerer.scope_name(name))
        .transpose()?;
    let function_length = function
        .parameter_initializers
        .iter()
        .position(Option::is_some)
        .unwrap_or(function.parameters.len());
    let environment_slots = freeze_environment_slot_metadata(captured_slots);
    let self_binding_slot = function.self_binding.as_ref().and_then(|binding| {
        captured_slots
            .iter()
            .find(|slot| slot.id == binding.id)
            .map(|slot| slot.slot)
    });
    debug_assert!(lowerer.suspend_points.len() <= function_capacity.suspend_points);
    let handlers = freeze_handlers(lowerer.handlers)?;
    let suspend_points = lowerer.suspend_points.into();
    let binding_plan = lowerer.binding_plan.into();
    // Unused parameters own frame registers even when no instruction mentions them. The bytecode
    // builder only observes encoded operands, so retain the lowerer's complete allocation frontier.
    let allocated_register_count = lowerer.next_register;
    let (bytecode, source_map, encoded_register_count) =
        lowerer.builder.finish().map_err(CompileError::Builder)?;
    let register_count = encoded_register_count.max(allocated_register_count);
    let function_id = function
        .id
        .index()
        .checked_add(1)
        .map(FunctionId::new)
        .ok_or(CompileError::RegisterOverflow)?;
    let metadata = FunctionMetadata {
        kind: match function.kind {
            HirFunctionKind::Ordinary => FunctionKind::Ordinary,
            HirFunctionKind::Async => FunctionKind::Async,
            HirFunctionKind::Generator => FunctionKind::Generator,
            HirFunctionKind::AsyncGenerator => FunctionKind::AsyncGenerator,
            HirFunctionKind::DerivedClassConstructor
            | HirFunctionKind::DefaultDerivedConstructor => FunctionKind::DerivedClassConstructor,
            HirFunctionKind::BaseClassConstructor | HirFunctionKind::DefaultBaseConstructor => {
                FunctionKind::BaseClassConstructor
            }
        },
        role: match function.role {
            crate::HirFunctionRole::Ordinary => tachyon_bytecode::FunctionRole::Ordinary,
            crate::HirFunctionRole::Method => tachyon_bytecode::FunctionRole::Method,
            crate::HirFunctionRole::ClassFieldInitializer => {
                tachyon_bytecode::FunctionRole::ClassFieldInitializer
            }
        },
        strictness: if function.strict {
            FunctionStrictness::Strict
        } else {
            FunctionStrictness::Sloppy
        },
        layout: FunctionLayout {
            register_count,
            argument_count: u32::try_from(function.parameters.len())
                .map_err(|_| CompileError::RegisterOverflow)?,
            function_length: u32::try_from(function_length)
                .map_err(|_| CompileError::RegisterOverflow)?,
            name_scope,
            max_handler_depth: function_capacity.max_handler_depth,
            max_completion_depth: function_capacity.max_completion_depth,
            environment_slot_count: u32::try_from(
                environments.functions[function_index].slots.len(),
            )
            .map_err(|_| CompileError::BindingOverflow)?,
            self_binding_slot,
            needs_argument_source: lowerer.needs_argument_source,
            has_rest_parameter: function.rest_parameter.is_some(),
            simple_parameter_list: function
                .parameters
                .iter()
                .all(|parameter| matches!(parameter.kind, crate::HirPatternKind::Binding(_))),
            ..FunctionLayout::default()
        },
        source_map,
        handlers,
        suspend_points,
        feedback_sites: Default::default(),
        binding_plan,
        environment_record_kind: EnvironmentRecordKind::Function,
        environment_slots,
    };
    let template = CompiledFunctionTemplate::new(function_id, bytecode, metadata);
    Ok(match function.source_span {
        Some(span) => template.with_source_span(tachyon_bytecode::SourceSpan {
            start: span.start,
            end: span.end,
        }),
        None => template,
    })
}

/// Freezes one exact-capacity owner metadata slice without changing bytecode or binding references.
fn freeze_environment_slot_metadata(
    slots: &[CapturedSlot],
) -> std::sync::Arc<[EnvironmentSlotMetadata]> {
    let mut metadata = Vec::with_capacity(slots.len());
    for slot in slots {
        metadata.push(EnvironmentSlotMetadata {
            name: slot.name.clone(),
            mutable: slot.mutable,
            initialized: slot.initialized,
            parameter: slot.parameter,
        });
    }
    debug_assert_eq!(metadata.len(), slots.len());
    metadata.into()
}

fn scope_strictness(
    source: &SourceText,
    hir: &HirProgram,
    scope: ScopeId,
    span: SourceSpan,
) -> Result<FunctionStrictness, CompileError> {
    let flags = hir
        .scopes()
        .iter()
        .find(|candidate| candidate.id == scope)
        .map(|scope| scope.flags)
        .ok_or_else(|| CompileError::MissingSemanticId {
            source_name: source.name().clone(),
            span,
            semantic: "scope strictness",
        })?;
    Ok(if flags.strict {
        FunctionStrictness::Strict
    } else {
        FunctionStrictness::Sloppy
    })
}

/// Collects function/script-scoped var names once with a source-derived no-growth capacity.
fn var_declared_bindings(
    statements: &[HirStatement],
) -> Result<Vec<crate::HirBinding>, CompileError> {
    let mut bindings = Vec::with_capacity(capacity::estimate_var_bindings(statements)?);
    collect_var_declared_bindings(statements, &mut bindings);
    Ok(bindings)
}

/// Walks statement containers but deliberately stops at separately owned function stencils.
fn collect_var_declared_bindings(
    statements: &[HirStatement],
    bindings: &mut Vec<crate::HirBinding>,
) {
    for statement in statements {
        match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => {
                if declaration.kind == HirVariableDeclarationKind::Var {
                    push_var_declaration_bindings(declaration, bindings);
                }
            }
            HirStatementKind::Block(statements) => {
                collect_var_declared_bindings(statements, bindings);
            }
            HirStatementKind::Labeled { body, .. } => {
                collect_var_declared_bindings(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                collect_var_declared_bindings(core::slice::from_ref(consequent), bindings);
                if let Some(alternate) = alternate {
                    collect_var_declared_bindings(core::slice::from_ref(alternate), bindings);
                }
            }
            HirStatementKind::For {
                initializer, body, ..
            } => {
                if let Some(HirForInitializer::Variable(declaration)) = initializer
                    && declaration.kind == HirVariableDeclarationKind::Var
                {
                    push_var_declaration_bindings(declaration, bindings);
                }
                collect_var_declared_bindings(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::ForIn { left, body, .. } => {
                if let HirForInLeft::Variable(declaration) = left
                    && declaration.kind == HirVariableDeclarationKind::Var
                {
                    push_var_declaration_bindings(declaration, bindings);
                }
                collect_var_declared_bindings(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::ForOf { left, body, .. } => {
                if let HirForInLeft::Variable(declaration) = left
                    && declaration.kind == HirVariableDeclarationKind::Var
                {
                    push_var_declaration_bindings(declaration, bindings);
                }
                collect_var_declared_bindings(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::Loop { body, .. } => {
                collect_var_declared_bindings(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::Switch { cases, .. } => {
                for case in cases.iter() {
                    collect_var_declared_bindings(&case.consequent, bindings);
                }
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                collect_var_declared_bindings(block, bindings);
                if let Some(handler) = handler {
                    collect_var_declared_bindings(&handler.body, bindings);
                }
                if let Some(finalizer) = finalizer {
                    collect_var_declared_bindings(finalizer, bindings);
                }
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::BreakLabeled(_)
            | HirStatementKind::ContinueLabeled(_)
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => {}
        }
    }
}

fn push_var_declaration_bindings(
    declaration: &HirVariableDeclaration,
    bindings: &mut Vec<crate::HirBinding>,
) {
    for declarator in declaration.declarators.iter() {
        for declarator_binding in pattern_bindings(&declarator.pattern) {
            if bindings
                .iter()
                .all(|binding| binding.id != declarator_binding.id)
            {
                bindings.push(declarator_binding);
            }
        }
    }
}

fn freeze_handlers(
    handlers: Vec<Option<HandlerEntry>>,
) -> Result<std::sync::Arc<[HandlerEntry]>, CompileError> {
    let mut frozen = Vec::with_capacity(handlers.len());
    for handler in handlers {
        frozen.push(handler.ok_or(CompileError::UnboundExceptionHandler)?);
    }
    Ok(frozen.into())
}
