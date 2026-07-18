//! Lowering of the first owned HIR subset into immutable register bytecode.

use tachyon_bytecode::{
    BindingLocation, BindingPlanEntry, BytecodeBuilder, BytecodeConstant, CompiledFunctionTemplate,
    CompiledModule, FunctionId, FunctionKind, FunctionLayout, FunctionMetadata, FunctionStrictness,
    HandlerEntry, HandlerKind, Label, MAX_ENCODED_INSTRUCTION_WORDS, Opcode, RegisterId,
    SourceSpan as BytecodeSourceSpan,
};

use crate::hir::{HirAssignmentOperator, HirAssignmentTarget};
use crate::{
    BindingId, CompileError, HirBinaryOperator, HirCatchClause, HirExpression, HirExpressionKind,
    HirForInitializer, HirFunction, HirFunctionDeclaration, HirIdentifierReference,
    HirLogicalOperator, HirObjectPropertyKey, HirProgram, HirStatement, HirStatementKind,
    HirSwitchCase, HirUnaryOperator, HirUpdateOperator, HirVariableDeclaration,
    HirVariableDeclarationKind, ProgramKind, ScopeId, SourceName, SourceSpan, SourceText,
};

/// Lowers the currently supported HIR subset while preallocating builder and constant-pool storage from HIR counts.
pub(crate) fn lower(source: &SourceText, hir: &HirProgram) -> Result<CompiledModule, CompileError> {
    let environments = EnvironmentPlans::new(hir)?;
    let mut constants = Vec::with_capacity(hir_literal_count(hir)?);
    let mut scope_names = Vec::with_capacity(hir_scope_name_capacity(hir)?);
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
    CompiledModule::new(
        source.shared_text(),
        constants,
        scope_names,
        templates,
        FunctionId::new(0),
    )
    .map_err(CompileError::Module)
}

/// Hoists top-level function declarations, then lowers script completion into function zero.
fn lower_entry(
    source: &SourceText,
    hir: &HirProgram,
    environments: &EnvironmentPlans,
    constants: &mut Vec<BytecodeConstant>,
    scope_names: &mut Vec<std::sync::Arc<str>>,
) -> Result<CompiledFunctionTemplate, CompileError> {
    let var_bindings = var_declared_bindings(hir.statements())?;
    let has_control_flow = hir.statements().iter().any(|statement| {
        matches!(
            statement.kind,
            HirStatementKind::Block(_)
                | HirStatementKind::If { .. }
                | HirStatementKind::For { .. }
                | HirStatementKind::Loop { .. }
                | HirStatementKind::Switch { .. }
                | HirStatementKind::Try { .. }
                | HirStatementKind::Break
                | HirStatementKind::Continue
                | HirStatementKind::Throw(_)
        )
    });
    let has_expression = hir
        .statements()
        .iter()
        .any(|statement| matches!(&statement.kind, HirStatementKind::Expression(_)));
    let result_instruction_count = if has_control_flow {
        statements_expression_count(hir.statements())?
            .checked_add(2)
            .ok_or(CompileError::LoweringCapacityOverflow {
                collection: "entry completion instructions",
            })?
    } else if has_expression {
        1
    } else {
        2
    };
    let instruction_upper_bound = hir_instruction_count(hir)?
        .checked_add(var_bindings.len())
        .and_then(|count| count.checked_add(environments.global_lexicals.len()))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "global var instantiation instructions",
        })?
        .checked_add(result_instruction_count)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "bytecode instructions",
        })?;
    let word_capacity = instruction_upper_bound
        .checked_mul(MAX_ENCODED_INSTRUCTION_WORDS)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "bytecode words",
        })?;
    let handler_count = statements_handler_count(hir.statements())?;
    let max_handler_depth = statements_handler_depth(hir.statements())?;
    let entry_binding_plan_capacity = hir_binding_count(hir)?
        .checked_add(statements_scope_name_count(hir.statements())?)
        .and_then(|count| count.checked_add(var_bindings.len()))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "entry binding plan",
        })?;
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(word_capacity, hir_label_count(hir)?),
        constants,
        scope_names,
        locals: Vec::with_capacity(hir_binding_count(hir)?),
        binding_plan: Vec::with_capacity(entry_binding_plan_capacity),
        break_targets: Vec::with_capacity(statements_switch_count(hir.statements())?),
        continue_targets: Vec::with_capacity(statements_loop_count(hir.statements())?),
        handlers: Vec::with_capacity(handler_count),
        next_register: 0,
        source_name: source.name().clone(),
        script_scope: true,
        root_scope: hir.root_scope(),
        function_scope: None,
        environments,
    };
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
            lowerer.function_declaration(declaration, statement.span)?;
        }
    }
    for binding in &var_bindings {
        let scope_name = lowerer.global_binding(&binding.name, true)?;
        lowerer.emit(
            Opcode::DeclareScope,
            &[scope_name],
            SourceSpan { start: 0, end: 0 },
        )?;
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
                        | HirStatementKind::If { .. }
                        | HirStatementKind::For { .. }
                        | HirStatementKind::Loop { .. }
                        | HirStatementKind::Switch { .. }
                        | HirStatementKind::Try { .. }
                        | HirStatementKind::Break
                        | HirStatementKind::Continue
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
    let handlers = freeze_handlers(lowerer.handlers)?;
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
        strictness: scope_strictness(
            source,
            hir,
            hir.root_scope(),
            SourceSpan { start: 0, end: 0 },
        )?,
        layout: FunctionLayout {
            register_count,
            max_handler_depth,
            ..FunctionLayout::default()
        },
        source_map,
        handlers,
        suspend_points: Default::default(),
        feedback_sites: Default::default(),
        binding_plan,
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
}

#[derive(Clone, Debug)]
struct FunctionEnvironmentPlan {
    scope: ScopeId,
    parent_function: Option<usize>,
    slots: Vec<CapturedSlot>,
}

#[derive(Clone, Debug)]
struct EnvironmentPlans {
    functions: Vec<FunctionEnvironmentPlan>,
    global_lexicals: Vec<GlobalLexicalPlan>,
}

#[derive(Clone, Debug)]
struct GlobalLexicalPlan {
    id: BindingId,
    name: std::sync::Arc<str>,
    mutable: bool,
    span: SourceSpan,
}

impl EnvironmentPlans {
    /// Assigns exact slots only to bindings whose semantic references cross a function boundary.
    fn new(hir: &HirProgram) -> Result<Self, CompileError> {
        let mut global_lexicals = Vec::with_capacity(statements_binding_count(hir.statements())?);
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
                if declarator.binding.scope == hir.root_scope() {
                    global_lexicals.push(GlobalLexicalPlan {
                        id: declarator.binding.id,
                        name: declarator.binding.name.clone(),
                        mutable: declaration.kind == HirVariableDeclarationKind::Let,
                        span: declarator.span,
                    });
                }
            }
        }
        let mut functions = Vec::with_capacity(hir.functions().len());
        for function in hir.functions() {
            let capacity = function
                .parameters
                .len()
                .checked_add(statements_binding_count(&function.body)?)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "captured environment slots",
                })?;
            let mut slots = Vec::with_capacity(capacity);
            for parameter in function.parameters.iter() {
                push_captured_slot(parameter, true, &mut slots)?;
            }
            collect_captured_slots(&function.body, &mut slots)?;
            functions.push(FunctionEnvironmentPlan {
                scope: function.scope,
                parent_function: None,
                slots,
            });
        }
        for index in 0..functions.len() {
            functions[index].parent_function =
                nearest_parent_function(functions[index].scope, hir.scopes(), &functions);
        }
        Ok(Self {
            functions,
            global_lexicals,
        })
    }

    #[inline(always)]
    fn local_slot(&self, function_scope: ScopeId, binding: BindingId) -> Option<&CapturedSlot> {
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

    /// Resolves a captured reference to the nearest allocated environment chain node.
    fn reference_slot(
        &self,
        current_scope: ScopeId,
        binding: BindingId,
    ) -> Option<(u32, &CapturedSlot)> {
        let current = self
            .functions
            .iter()
            .position(|function| function.scope == current_scope)?;
        let target = self
            .functions
            .iter()
            .position(|function| function.slots.iter().any(|slot| slot.id == binding))?;
        let mut cursor = if self.functions[current].slots.is_empty() {
            self.nearest_environment_parent(current)?
        } else {
            current
        };
        let mut depth = 0_u32;
        while cursor != target {
            cursor = self.nearest_environment_parent(cursor)?;
            depth = depth.checked_add(1)?;
        }
        let slot = self.functions[target]
            .slots
            .iter()
            .find(|slot| slot.id == binding)?;
        Some((depth, slot))
    }

    fn nearest_environment_parent(&self, mut function: usize) -> Option<usize> {
        loop {
            function = self.functions[function].parent_function?;
            if !self.functions[function].slots.is_empty() {
                return Some(function);
            }
        }
    }
}

fn nearest_parent_function(
    scope: ScopeId,
    scopes: &[crate::HirScope],
    functions: &[FunctionEnvironmentPlan],
) -> Option<usize> {
    let mut parent = scopes.get(scope.index() as usize)?.parent;
    while let Some(scope) = parent {
        if let Some(index) = functions
            .iter()
            .position(|function| function.scope == scope)
        {
            return Some(index);
        }
        parent = scopes.get(scope.index() as usize)?.parent;
    }
    None
}

fn push_captured_slot(
    binding: &crate::HirBinding,
    mutable: bool,
    slots: &mut Vec<CapturedSlot>,
) -> Result<(), CompileError> {
    if !binding.captured || slots.iter().any(|slot| slot.id == binding.id) {
        return Ok(());
    }
    let slot = u32::try_from(slots.len()).map_err(|_| CompileError::BindingOverflow)?;
    slots.push(CapturedSlot {
        id: binding.id,
        slot,
        name: binding.name.clone(),
        mutable,
    });
    Ok(())
}

/// Walks activation-owned declarations while nested function bodies remain separate stencils.
fn collect_captured_slots(
    statements: &[HirStatement],
    slots: &mut Vec<CapturedSlot>,
) -> Result<(), CompileError> {
    for statement in statements {
        match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => {
                let mutable = declaration.kind != HirVariableDeclarationKind::Const;
                for declarator in declaration.declarators.iter() {
                    push_captured_slot(&declarator.binding, mutable, slots)?;
                }
            }
            HirStatementKind::FunctionDeclaration(declaration) => {
                push_captured_slot(&declaration.binding, true, slots)?;
            }
            HirStatementKind::Block(body) => collect_captured_slots(body, slots)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                collect_captured_slots(core::slice::from_ref(consequent), slots)?;
                if let Some(alternate) = alternate {
                    collect_captured_slots(core::slice::from_ref(alternate), slots)?;
                }
            }
            HirStatementKind::For {
                initializer, body, ..
            } => {
                if let Some(HirForInitializer::Variable(declaration)) = initializer {
                    let mutable = declaration.kind != HirVariableDeclarationKind::Const;
                    for declarator in declaration.declarators.iter() {
                        push_captured_slot(&declarator.binding, mutable, slots)?;
                    }
                }
                collect_captured_slots(core::slice::from_ref(body), slots)?;
            }
            HirStatementKind::Loop { body, .. } => {
                collect_captured_slots(core::slice::from_ref(body), slots)?;
            }
            HirStatementKind::Switch { cases, .. } => {
                for case in cases.iter() {
                    collect_captured_slots(&case.consequent, slots)?;
                }
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                collect_captured_slots(block, slots)?;
                if let Some(handler) = handler {
                    if let Some(parameter) = &handler.parameter {
                        push_captured_slot(parameter, true, slots)?;
                    }
                    collect_captured_slots(&handler.body, slots)?;
                }
                if let Some(finalizer) = finalizer {
                    collect_captured_slots(finalizer, slots)?;
                }
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
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
            !function
                .parameters
                .iter()
                .any(|parameter| parameter.id == binding.id)
        })
        .count();
    let parameter_initializer_count = function
        .parameter_initializers
        .iter()
        .filter(|initializer| initializer.is_some())
        .count();
    let parameter_initializer_instructions =
        parameter_initializer_instruction_count(&function.parameter_initializers)?;
    let instruction_capacity = statements_instruction_count(&function.body)?
        .checked_add(var_initialization_count)
        .and_then(|count| count.checked_add(function.parameters.len()))
        .and_then(|count| count.checked_add(parameter_initializer_instructions))
        .and_then(|count| count.checked_add(2))
        .and_then(|count| count.checked_mul(MAX_ENCODED_INSTRUCTION_WORDS))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function bytecode words",
        })?;
    let handler_count = statements_handler_count(&function.body)?;
    let max_handler_depth = statements_handler_depth(&function.body)?;
    let local_binding_capacity = function
        .parameters
        .len()
        .checked_add(statements_binding_count(&function.body)?)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function local bindings",
        })?;
    let parameter_scope_names = parameter_scope_name_count(&function.parameter_initializers)?;
    let binding_plan_capacity = local_binding_capacity
        .checked_add(statements_scope_name_count(&function.body)?)
        .and_then(|count| count.checked_add(parameter_scope_names))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function binding plan",
        })?;
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(
            instruction_capacity,
            statements_label_count(&function.body)?
                .checked_add(parameter_initializer_count)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "bytecode labels",
                })?,
        ),
        constants,
        scope_names,
        locals: Vec::with_capacity(local_binding_capacity),
        binding_plan: Vec::with_capacity(binding_plan_capacity),
        break_targets: Vec::with_capacity(statements_switch_count(&function.body)?),
        continue_targets: Vec::with_capacity(statements_loop_count(&function.body)?),
        handlers: Vec::with_capacity(handler_count),
        next_register: 0,
        source_name: source.name().clone(),
        script_scope: false,
        root_scope,
        function_scope: Some(function.scope),
        environments,
    };
    for parameter in function.parameters.iter() {
        let register = lowerer.register()?;
        lowerer.add_local(parameter, Some(register), true)?;
    }
    for (index, initializer) in function.parameter_initializers.iter().enumerate() {
        if let Some(initializer) = initializer {
            let parameter =
                RegisterId::new(u32::try_from(index).map_err(|_| CompileError::RegisterOverflow)?);
            lowerer.parameter_initializer(parameter, initializer)?;
        }
    }
    for binding in &var_bindings {
        if lowerer.local_by_id(binding.id).is_some() {
            continue;
        }
        if binding.captured {
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
    let mut terminal = false;
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
    let handlers = freeze_handlers(lowerer.handlers)?;
    let binding_plan = lowerer.binding_plan.into();
    let (bytecode, source_map, register_count) =
        lowerer.builder.finish().map_err(CompileError::Builder)?;
    let function_id = function
        .id
        .index()
        .checked_add(1)
        .map(FunctionId::new)
        .ok_or(CompileError::RegisterOverflow)?;
    Ok(CompiledFunctionTemplate::new(
        function_id,
        bytecode,
        FunctionMetadata {
            kind: FunctionKind::Ordinary,
            strictness: if function.strict {
                FunctionStrictness::Strict
            } else {
                FunctionStrictness::Sloppy
            },
            layout: FunctionLayout {
                register_count,
                argument_count: u32::try_from(function.parameters.len())
                    .map_err(|_| CompileError::RegisterOverflow)?,
                max_handler_depth,
                environment_slot_count: u32::try_from(
                    environments.functions[function_index].slots.len(),
                )
                .map_err(|_| CompileError::BindingOverflow)?,
                ..FunctionLayout::default()
            },
            source_map,
            handlers,
            suspend_points: Default::default(),
            feedback_sites: Default::default(),
            binding_plan,
        },
    ))
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

struct Lowerer<'a> {
    builder: BytecodeBuilder,
    constants: &'a mut Vec<BytecodeConstant>,
    scope_names: &'a mut Vec<std::sync::Arc<str>>,
    locals: Vec<LocalBinding>,
    binding_plan: Vec<BindingPlanEntry>,
    break_targets: Vec<Label>,
    continue_targets: Vec<Label>,
    handlers: Vec<Option<HandlerEntry>>,
    next_register: u32,
    source_name: SourceName,
    script_scope: bool,
    root_scope: ScopeId,
    function_scope: Option<ScopeId>,
    environments: &'a EnvironmentPlans,
}

#[derive(Clone, Debug)]
struct LocalBinding {
    id: BindingId,
    storage: LocalStorage,
    mutable: bool,
}

#[derive(Clone, Copy, Debug)]
enum LocalStorage {
    Register(RegisterId),
    Environment { depth: u32, slot: u32 },
}

impl Lowerer<'_> {
    /// Allocates a fresh register and emits one instruction with the HIR span copied into bytecode source metadata.
    fn emit(
        &mut self,
        opcode: Opcode,
        operands: &[u32],
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        self.builder
            .emit(
                opcode,
                operands,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map(|_| ())
            .map_err(CompileError::Builder)
    }

    /// Publishes one hoisted function binding before any top-level statement can reference it.
    fn function_declaration(
        &mut self,
        declaration: &HirFunctionDeclaration,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let register = self.register()?;
        let function = declaration
            .function
            .index()
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        self.emit(Opcode::CreateClosure, &[register.index(), function], span)?;
        let scope_name = self.global_binding(&declaration.binding.name, true)?;
        self.emit(Opcode::StoreScope, &[register.index(), scope_name], span)?;
        Ok(())
    }

    /// Instantiates one direct function-body declaration before ordinary statement execution.
    fn local_function_declaration(
        &mut self,
        declaration: &HirFunctionDeclaration,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let register = self.register()?;
        let function = declaration
            .function
            .index()
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        self.emit(Opcode::CreateClosure, &[register.index(), function], span)?;
        self.add_local(&declaration.binding, Some(register), true)
    }

    /// Emits one parameter default prologue, applying it only when the argument is undefined.
    fn parameter_initializer(
        &mut self,
        parameter: RegisterId,
        initializer: &HirExpression,
    ) -> Result<(), CompileError> {
        let undefined = self.load_undefined(initializer.span)?;
        let is_undefined = self.register()?;
        self.emit(
            Opcode::StrictEqual,
            &[is_undefined.index(), parameter.index(), undefined.index()],
            initializer.span,
        )?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .emit_jump_if_false(
                is_undefined,
                end,
                BytecodeSourceSpan {
                    start: initializer.span.start,
                    end: initializer.span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let value = self.expression(initializer)?;
        self.emit(
            Opcode::Move,
            &[parameter.index(), value.index()],
            initializer.span,
        )?;
        self.builder.bind_label(end).map_err(CompileError::Builder)
    }

    /// Lowers one script statement while preserving the most recent non-empty completion value.
    fn entry_statement(
        &mut self,
        statement: &HirStatement,
        result: RegisterId,
    ) -> Result<bool, CompileError> {
        match &statement.kind {
            HirStatementKind::Expression(expression) => {
                let value = self.expression(expression)?;
                self.emit(
                    Opcode::Move,
                    &[result.index(), value.index()],
                    statement.span,
                )?;
                Ok(false)
            }
            HirStatementKind::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration)?;
                Ok(false)
            }
            HirStatementKind::FunctionDeclaration(_) | HirStatementKind::Empty => Ok(false),
            HirStatementKind::Throw(argument) => {
                let value = self.expression(argument)?;
                self.emit(Opcode::Throw, &[value.index()], statement.span)?;
                Ok(true)
            }
            HirStatementKind::Block(statements) => {
                let checkpoint = self.locals.len();
                let mut terminal = false;
                for statement in statements.iter() {
                    terminal = self.entry_statement(statement, result)?;
                    if terminal {
                        break;
                    }
                }
                self.locals.truncate(checkpoint);
                Ok(terminal)
            }
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => self.entry_if_statement(
                test,
                consequent,
                alternate.as_deref(),
                result,
                statement.span,
            ),
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                self.entry_for_statement(
                    initializer.as_ref(),
                    test.as_ref(),
                    update.as_ref(),
                    body,
                    result,
                    statement.span,
                )?;
                Ok(false)
            }
            HirStatementKind::Loop {
                test,
                body,
                test_first,
            } => {
                self.entry_loop_statement(test, body, result, *test_first, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                self.entry_switch_statement(discriminant, cases, result, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => self.entry_try_statement(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                result,
                statement.span,
            ),
            HirStatementKind::Break => {
                let target = self.current_break_target(statement.span)?;
                self.emit_jump(target, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Continue => {
                let target = self.current_continue_target(statement.span)?;
                self.emit_jump(target, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Return(_) => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: statement.span,
                syntax: "top-level return",
            }),
        }
    }

    /// Emits a script conditional and updates the shared completion register only in executed arms.
    fn entry_if_statement(
        &mut self,
        test: &HirExpression,
        consequent: &HirStatement,
        alternate: Option<&HirStatement>,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        let test = self.expression(test)?;
        let alternate_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let bytecode_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        self.builder
            .emit_jump_if_false(test, alternate_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        let consequent_terminal = self.entry_statement(consequent, result)?;
        self.builder
            .emit_jump(end_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        self.builder
            .bind_label(alternate_label)
            .map_err(CompileError::Builder)?;
        let alternate_terminal = alternate
            .map(|alternate| self.entry_statement(alternate, result))
            .transpose()?
            .unwrap_or(false);
        self.builder
            .bind_label(end_label)
            .map_err(CompileError::Builder)?;
        Ok(alternate.is_some() && consequent_terminal && alternate_terminal)
    }

    /// Emits a classic script for-loop while preserving completion and update-before-continue flow.
    fn entry_for_statement(
        &mut self,
        initializer: Option<&HirForInitializer>,
        test: Option<&HirExpression>,
        update: Option<&HirExpression>,
        body: &HirStatement,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        self.for_initializer(initializer)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let update_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        if let Some(test) = test {
            let test = self.expression(test)?;
            self.builder
                .emit_jump_if_false(
                    test,
                    end,
                    BytecodeSourceSpan {
                        start: span.start,
                        end: span.end,
                    },
                )
                .map_err(CompileError::Builder)?;
        }
        self.break_targets.push(end);
        self.continue_targets.push(update_label);
        self.entry_statement(body, result)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(update_label)
            .map_err(CompileError::Builder)?;
        if let Some(update) = update {
            self.expression(update)?;
        }
        self.emit_jump(condition, span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.restore_for_scope(initializer, checkpoint);
        Ok(())
    }

    /// Emits a script while/do-while loop with the condition as the continue destination.
    fn entry_loop_statement(
        &mut self,
        test: &HirExpression,
        body: &HirStatement,
        result: RegisterId,
        test_first: bool,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let body_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if test_first {
            self.emit_jump(condition, span)?;
        }
        self.builder
            .bind_label(body_label)
            .map_err(CompileError::Builder)?;
        self.break_targets.push(end);
        self.continue_targets.push(condition);
        self.entry_statement(body, result)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        let test = self.expression(test)?;
        self.builder
            .emit_jump_if_true(
                test,
                body_label,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        self.builder.bind_label(end).map_err(CompileError::Builder)
    }

    /// Lowers one function-body statement and reports whether it ends in an abrupt completion.
    fn function_statement(&mut self, statement: &HirStatement) -> Result<bool, CompileError> {
        match &statement.kind {
            HirStatementKind::Expression(expression) => {
                self.expression(expression)?;
                Ok(false)
            }
            HirStatementKind::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration)?;
                Ok(false)
            }
            HirStatementKind::Return(argument) => {
                if let Some(argument) = argument {
                    let value = self.expression(argument)?;
                    self.emit(Opcode::Return, &[value.index()], statement.span)?;
                } else {
                    self.emit(Opcode::ReturnUndefined, &[], statement.span)?;
                }
                Ok(true)
            }
            HirStatementKind::Throw(argument) => {
                let value = self.expression(argument)?;
                self.emit(Opcode::Throw, &[value.index()], statement.span)?;
                Ok(true)
            }
            HirStatementKind::Block(statements) => {
                let checkpoint = self.locals.len();
                let mut terminal = false;
                for statement in statements.iter() {
                    terminal = self.function_statement(statement)?;
                    if terminal {
                        break;
                    }
                }
                self.locals.truncate(checkpoint);
                Ok(terminal)
            }
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                self.if_statement(test, consequent, alternate.as_deref(), statement.span)?;
                Ok(false)
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                self.function_for_statement(
                    initializer.as_ref(),
                    test.as_ref(),
                    update.as_ref(),
                    body,
                    statement.span,
                )?;
                Ok(false)
            }
            HirStatementKind::Loop {
                test,
                body,
                test_first,
            } => {
                self.function_loop_statement(test, body, *test_first, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                self.function_switch_statement(discriminant, cases, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => self.function_try_statement(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                statement.span,
            ),
            HirStatementKind::Break => {
                let target = self.current_break_target(statement.span)?;
                self.emit_jump(target, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Continue => {
                let target = self.current_continue_target(statement.span)?;
                self.emit_jump(target, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Empty => Ok(false),
            HirStatementKind::FunctionDeclaration(_) => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: statement.span,
                syntax: "nested function declaration",
            }),
        }
    }

    /// Emits a classic function-body for-loop with explicit break and continue label stacks.
    fn function_for_statement(
        &mut self,
        initializer: Option<&HirForInitializer>,
        test: Option<&HirExpression>,
        update: Option<&HirExpression>,
        body: &HirStatement,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        self.for_initializer(initializer)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let update_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        if let Some(test) = test {
            let test = self.expression(test)?;
            self.builder
                .emit_jump_if_false(
                    test,
                    end,
                    BytecodeSourceSpan {
                        start: span.start,
                        end: span.end,
                    },
                )
                .map_err(CompileError::Builder)?;
        }
        self.break_targets.push(end);
        self.continue_targets.push(update_label);
        self.function_statement(body)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(update_label)
            .map_err(CompileError::Builder)?;
        if let Some(update) = update {
            self.expression(update)?;
        }
        self.emit_jump(condition, span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.restore_for_scope(initializer, checkpoint);
        Ok(())
    }

    /// Emits an ordinary-function while/do-while loop without entering the Rust call stack.
    fn function_loop_statement(
        &mut self,
        test: &HirExpression,
        body: &HirStatement,
        test_first: bool,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let body_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if test_first {
            self.emit_jump(condition, span)?;
        }
        self.builder
            .bind_label(body_label)
            .map_err(CompileError::Builder)?;
        self.break_targets.push(end);
        self.continue_targets.push(condition);
        self.function_statement(body)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        let test = self.expression(test)?;
        self.builder
            .emit_jump_if_true(
                test,
                body_label,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        self.builder.bind_label(end).map_err(CompileError::Builder)
    }

    fn for_initializer(
        &mut self,
        initializer: Option<&HirForInitializer>,
    ) -> Result<(), CompileError> {
        match initializer {
            Some(HirForInitializer::Variable(declaration)) => {
                self.variable_declaration(declaration)
            }
            Some(HirForInitializer::Expression(expression)) => {
                self.expression(expression).map(|_| ())
            }
            None => Ok(()),
        }
    }

    fn restore_for_scope(&mut self, initializer: Option<&HirForInitializer>, checkpoint: usize) {
        if let Some(HirForInitializer::Variable(declaration)) = initializer
            && !matches!(declaration.kind, HirVariableDeclarationKind::Var)
        {
            self.locals.truncate(checkpoint);
        }
    }

    /// Emits a structured conditional while leaving both lexical branches to the statement lowerer.
    fn if_statement(
        &mut self,
        test: &HirExpression,
        consequent: &HirStatement,
        alternate: Option<&HirStatement>,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let test = self.expression(test)?;
        let alternate_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let bytecode_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        self.builder
            .emit_jump_if_false(test, alternate_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        self.function_statement(consequent)?;
        self.builder
            .emit_jump(end_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        self.builder
            .bind_label(alternate_label)
            .map_err(CompileError::Builder)?;
        if let Some(alternate) = alternate {
            self.function_statement(alternate)?;
        }
        self.builder
            .bind_label(end_label)
            .map_err(CompileError::Builder)
    }

    /// Lowers a script try/catch into one immutable range while sharing UpdateEmpty state.
    fn entry_try_statement(
        &mut self,
        block: &[HirStatement],
        handler: Option<&HirCatchClause>,
        finalizer: Option<&[HirStatement]>,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        if finalizer.is_some() {
            return Err(self.unsupported(span, "finally statement"));
        }
        let handler = handler.ok_or_else(|| self.unsupported(span, "try without catch"))?;
        let handler_slot = self.reserve_handler();
        let protected_start = self.emit_marker(span)?;
        let try_terminal = self.entry_statement_list(block, result)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if !try_terminal {
            self.emit_jump(end, span)?;
        }
        let checkpoint = self.locals.len();
        let handler_offset = self.emit_catch_binding(handler)?;
        let catch_terminal = self.entry_statement_list(&handler.body, result)?;
        self.locals.truncate(checkpoint);
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.publish_catch_handler(handler_slot, protected_start, handler_offset)?;
        Ok(try_terminal && catch_terminal)
    }

    /// Lowers an ordinary-function try/catch with identical handler and lexical checkpoints.
    fn function_try_statement(
        &mut self,
        block: &[HirStatement],
        handler: Option<&HirCatchClause>,
        finalizer: Option<&[HirStatement]>,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        if finalizer.is_some() {
            return Err(self.unsupported(span, "finally statement"));
        }
        let handler = handler.ok_or_else(|| self.unsupported(span, "try without catch"))?;
        let handler_slot = self.reserve_handler();
        let protected_start = self.emit_marker(span)?;
        let try_terminal = self.function_statement_list(block)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if !try_terminal {
            self.emit_jump(end, span)?;
        }
        let checkpoint = self.locals.len();
        let handler_offset = self.emit_catch_binding(handler)?;
        let catch_terminal = self.function_statement_list(&handler.body)?;
        self.locals.truncate(checkpoint);
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.publish_catch_handler(handler_slot, protected_start, handler_offset)?;
        Ok(try_terminal && catch_terminal)
    }

    fn entry_statement_list(
        &mut self,
        statements: &[HirStatement],
        result: RegisterId,
    ) -> Result<bool, CompileError> {
        let checkpoint = self.locals.len();
        let mut terminal = false;
        for statement in statements {
            terminal = self.entry_statement(statement, result)?;
            if terminal {
                break;
            }
        }
        self.locals.truncate(checkpoint);
        Ok(terminal)
    }

    fn function_statement_list(
        &mut self,
        statements: &[HirStatement],
    ) -> Result<bool, CompileError> {
        let checkpoint = self.locals.len();
        let mut terminal = false;
        for statement in statements {
            terminal = self.function_statement(statement)?;
            if terminal {
                break;
            }
        }
        self.locals.truncate(checkpoint);
        Ok(terminal)
    }

    /// Emits the handler entry and optionally binds its pending exception register lexically.
    fn emit_catch_binding(
        &mut self,
        handler: &HirCatchClause,
    ) -> Result<tachyon_bytecode::WordOffset, CompileError> {
        let exception = self.register()?;
        let offset = self
            .builder
            .emit(
                Opcode::LoadException,
                &[exception.index()],
                BytecodeSourceSpan {
                    start: handler.span.start,
                    end: handler.span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        if let Some(parameter) = &handler.parameter {
            self.add_local(parameter, Some(exception), true)?;
        }
        Ok(offset)
    }

    fn reserve_handler(&mut self) -> usize {
        let index = self.handlers.len();
        self.handlers.push(None);
        index
    }

    fn emit_marker(
        &mut self,
        span: SourceSpan,
    ) -> Result<tachyon_bytecode::WordOffset, CompileError> {
        self.builder
            .emit(
                Opcode::Nop,
                &[],
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)
    }

    fn publish_catch_handler(
        &mut self,
        slot: usize,
        protected_start: tachyon_bytecode::WordOffset,
        handler: tachyon_bytecode::WordOffset,
    ) -> Result<(), CompileError> {
        let entry = HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        };
        *self
            .handlers
            .get_mut(slot)
            .ok_or(CompileError::UnboundExceptionHandler)? = Some(entry);
        Ok(())
    }

    fn unsupported(&self, span: SourceSpan, syntax: &'static str) -> CompileError {
        CompileError::UnsupportedSyntax {
            source_name: self.source_name.clone(),
            span,
            syntax,
        }
    }

    /// Emits switch dispatch and script clause bodies while preserving UpdateEmpty completion state.
    fn entry_switch_statement(
        &mut self,
        discriminant: &HirExpression,
        cases: &[HirSwitchCase],
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        let (case_labels, end) = self.emit_switch_dispatch(discriminant, cases, span)?;
        self.break_targets.push(end);
        for (case, label) in cases.iter().zip(case_labels) {
            self.builder
                .bind_label(label)
                .map_err(CompileError::Builder)?;
            for statement in case.consequent.iter() {
                if self.entry_statement(statement, result)? {
                    break;
                }
            }
        }
        self.break_targets.pop();
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.locals.truncate(checkpoint);
        Ok(())
    }

    /// Emits switch dispatch and ordinary-function clause bodies with source-order fallthrough.
    fn function_switch_statement(
        &mut self,
        discriminant: &HirExpression,
        cases: &[HirSwitchCase],
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        let (case_labels, end) = self.emit_switch_dispatch(discriminant, cases, span)?;
        self.break_targets.push(end);
        for (case, label) in cases.iter().zip(case_labels) {
            self.builder
                .bind_label(label)
                .map_err(CompileError::Builder)?;
            for statement in case.consequent.iter() {
                if self.function_statement(statement)? {
                    break;
                }
            }
        }
        self.break_targets.pop();
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.locals.truncate(checkpoint);
        Ok(())
    }

    /// Evaluates case tests in source order and returns exact labels for contiguous clause bodies.
    fn emit_switch_dispatch(
        &mut self,
        discriminant: &HirExpression,
        cases: &[HirSwitchCase],
        span: SourceSpan,
    ) -> Result<(Vec<Label>, Label), CompileError> {
        let discriminant_value = self.expression(discriminant)?;
        let discriminant = self.register()?;
        self.emit(
            Opcode::Move,
            &[discriminant.index(), discriminant_value.index()],
            span,
        )?;
        let mut labels = Vec::with_capacity(cases.len());
        for _ in cases {
            labels.push(self.builder.new_label().map_err(CompileError::Builder)?);
        }
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        let mut default = None;
        for (case, label) in cases.iter().zip(labels.iter().copied()) {
            let Some(test) = case.test.as_ref() else {
                default = Some(label);
                continue;
            };
            let test = self.expression(test)?;
            let equal = self.register()?;
            self.emit(
                Opcode::StrictEqual,
                &[equal.index(), discriminant.index(), test.index()],
                case.span,
            )?;
            self.builder
                .emit_jump_if_true(
                    equal,
                    label,
                    BytecodeSourceSpan {
                        start: case.span.start,
                        end: case.span.end,
                    },
                )
                .map_err(CompileError::Builder)?;
        }
        self.emit_jump(default.unwrap_or(end), span)?;
        Ok((labels, end))
    }

    fn emit_jump(&mut self, target: Label, span: SourceSpan) -> Result<(), CompileError> {
        self.builder
            .emit_jump(
                target,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map(|_| ())
            .map_err(CompileError::Builder)
    }

    fn current_break_target(&self, span: SourceSpan) -> Result<Label, CompileError> {
        self.break_targets
            .last()
            .copied()
            .ok_or_else(|| CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span,
                syntax: "break outside breakable statement",
            })
    }

    fn current_continue_target(&self, span: SourceSpan) -> Result<Label, CompileError> {
        self.continue_targets
            .last()
            .copied()
            .ok_or_else(|| self.unsupported(span, "continue outside loop"))
    }

    /// Lowers expressions into registers while leaving unsupported reference semantics as explicit errors.
    fn expression(&mut self, expression: &HirExpression) -> Result<RegisterId, CompileError> {
        match &expression.kind {
            HirExpressionKind::Number(bits) => {
                let value = f64::from_bits(*bits);
                if value.is_finite()
                    && value.fract() == 0.0
                    && value >= i32::MIN as f64
                    && value <= i32::MAX as f64
                {
                    self.load_immediate(value as i32 as u32, expression.span)
                } else {
                    let register = self.register()?;
                    let constant = u32::try_from(self.constants.len())
                        .map_err(|_| CompileError::ConstantOverflow)?;
                    self.constants.push(BytecodeConstant::NumberBits(*bits));
                    self.emit(
                        Opcode::LoadConstant,
                        &[register.index(), constant],
                        expression.span,
                    )?;
                    Ok(register)
                }
            }
            HirExpressionKind::String(value) => {
                let code_unit_count = value.encode_utf16().count();
                let mut code_units = Vec::new();
                code_units
                    .try_reserve_exact(code_unit_count)
                    .map_err(|_| CompileError::ConstantAllocationFailed)?;
                code_units.extend(value.encode_utf16());
                let constant = u32::try_from(self.constants.len())
                    .map_err(|_| CompileError::ConstantOverflow)?;
                self.constants
                    .push(BytecodeConstant::string_from_utf16(code_units));
                let destination = self.register()?;
                self.emit(
                    Opcode::LoadConstant,
                    &[destination.index(), constant],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Boolean(value) => self.load_boolean(*value, expression.span),
            HirExpressionKind::Null => self.load_null(expression.span),
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Not,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::Not,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Negate,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::Negate,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Typeof,
                argument,
            } => {
                if let HirExpressionKind::Identifier(reference) = &argument.kind
                    && self.local_reference(reference).is_none()
                    && self.captured_reference(reference)?.is_none()
                {
                    self.require_global_reference(reference, expression.span)?;
                    let destination = self.register()?;
                    let scope_name = self.resolved_global_binding(reference, false)?;
                    self.emit(
                        Opcode::TypeofScope,
                        &[destination.index(), scope_name],
                        expression.span,
                    )?;
                    return Ok(destination);
                }
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::Typeof,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Void,
                argument,
            } => {
                self.expression(argument)?;
                self.load_undefined(expression.span)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Delete,
                argument,
            } => match &argument.kind {
                HirExpressionKind::StaticMember { object, property } => {
                    let receiver = self.expression(object)?;
                    let destination = self.register()?;
                    let property = self.scope_name(property)?;
                    self.emit(
                        Opcode::DeleteById,
                        &[destination.index(), receiver.index(), property],
                        expression.span,
                    )?;
                    Ok(destination)
                }
                HirExpressionKind::ComputedMember { object, property } => {
                    let receiver = self.expression(object)?;
                    let property = self.expression(property)?;
                    let destination = self.register()?;
                    self.emit(
                        Opcode::DeleteByValue,
                        &[destination.index(), receiver.index(), property.index()],
                        expression.span,
                    )?;
                    Ok(destination)
                }
                HirExpressionKind::Identifier(_) => self.load_boolean(true, expression.span),
                _ => {
                    self.expression(argument)?;
                    self.load_boolean(true, expression.span)
                }
            },
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Plus,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::ToNumber,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::BitwiseNot,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::BitwiseNot,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                self.emit_binary(*operator, left, right, expression.span)
            }
            HirExpressionKind::Logical {
                operator,
                left,
                right,
            } => self.logical(*operator, left, right, expression.span),
            HirExpressionKind::Identifier(reference) => {
                match self.local_reference(reference).cloned() {
                    Some(binding) => self.read_local(&binding, expression.span),
                    None => {
                        if let Some(binding) = self.captured_reference(reference)? {
                            return self.read_local(&binding, expression.span);
                        }
                        self.require_global_reference(reference, expression.span)?;
                        let destination = self.register()?;
                        let scope_name = self.resolved_global_binding(reference, true)?;
                        self.emit(
                            Opcode::LoadScope,
                            &[destination.index(), scope_name],
                            expression.span,
                        )?;
                        Ok(destination)
                    }
                }
            }
            HirExpressionKind::Function(function) => {
                let destination = self.register()?;
                let function = function
                    .index()
                    .checked_add(1)
                    .ok_or(CompileError::RegisterOverflow)?;
                self.emit(
                    Opcode::CreateClosure,
                    &[destination.index(), function],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::This => {
                let destination = self.register()?;
                self.emit(Opcode::LoadThis, &[destination.index()], expression.span)?;
                Ok(destination)
            }
            HirExpressionKind::NewTarget => {
                let destination = self.register()?;
                self.emit(
                    Opcode::LoadNewTarget,
                    &[destination.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Object(properties) => {
                let object = self.register()?;
                self.emit(Opcode::CreateObject, &[object.index()], expression.span)?;
                for property in properties.iter() {
                    let (opcode, key) = match &property.key {
                        HirObjectPropertyKey::Static(key) => {
                            (Opcode::SetById, self.scope_name(key)?)
                        }
                        HirObjectPropertyKey::Computed(key) => {
                            (Opcode::SetByValue, self.expression(key)?.index())
                        }
                    };
                    let value = self.expression(&property.value)?;
                    self.emit(opcode, &[object.index(), value.index(), key], property.span)?;
                }
                Ok(object)
            }
            HirExpressionKind::StaticMember { object, property } => {
                let receiver = self.expression(object)?;
                let destination = self.register()?;
                let property = self.scope_name(property)?;
                self.emit(
                    Opcode::GetById,
                    &[destination.index(), receiver.index(), property],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::ComputedMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.expression(property)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::GetByValue,
                    &[destination.index(), receiver.index(), property.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Assignment {
                operator,
                target,
                value,
            } => self.assignment_expression(*operator, target, value, expression.span),
            HirExpressionKind::Update {
                operator,
                prefix,
                target,
            } => self.update_expression(*operator, *prefix, target, expression.span),
            HirExpressionKind::Conditional {
                test,
                consequent,
                alternate,
            } => self.conditional(test, consequent, alternate, expression.span),
            HirExpressionKind::Call { callee, arguments } => {
                self.call_expression(callee, arguments, expression.span)
            }
            HirExpressionKind::New { callee, arguments } => {
                self.construct_expression(callee, arguments, expression.span)
            }
        }
    }

    /// Reads a compound target before its RHS, computes once, and publishes the resulting value.
    fn assignment_expression(
        &mut self,
        operator: HirAssignmentOperator,
        target: &HirAssignmentTarget,
        value: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        match target {
            HirAssignmentTarget::Identifier(target) => {
                if let Some(binding) = self.local_reference(target).cloned() {
                    if !binding.mutable {
                        return Err(self.unsupported(span, "assignment to immutable local"));
                    }
                    let result = match operator {
                        HirAssignmentOperator::Assign => self.expression(value)?,
                        HirAssignmentOperator::Binary(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            let right = self.expression(value)?;
                            self.emit_binary(operator, old_value, right, span)?
                        }
                        HirAssignmentOperator::Logical(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            self.logical_assignment(operator, old_value, value, span, None)?
                        }
                    };
                    self.write_local(&binding, result, span)?;
                    return Ok(result);
                }
                if let Some(binding) = self.captured_reference(target)? {
                    if !binding.mutable {
                        return Err(self.unsupported(span, "assignment to immutable capture"));
                    }
                    let result = match operator {
                        HirAssignmentOperator::Assign => self.expression(value)?,
                        HirAssignmentOperator::Binary(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            let right = self.expression(value)?;
                            self.emit_binary(operator, old_value, right, span)?
                        }
                        HirAssignmentOperator::Logical(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            self.logical_assignment(operator, old_value, value, span, None)?
                        }
                    };
                    self.write_local(&binding, result, span)?;
                    return Ok(result);
                }
                self.scope_assignment(operator, target, value, span)
            }
            HirAssignmentTarget::StaticMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.scope_name(property)?;
                let result = match operator {
                    HirAssignmentOperator::Assign => self.expression(value)?,
                    HirAssignmentOperator::Binary(operator) => {
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetById,
                            &[old_value.index(), receiver.index(), property],
                            span,
                        )?;
                        let right = self.expression(value)?;
                        self.emit_binary(operator, old_value, right, span)?
                    }
                    HirAssignmentOperator::Logical(operator) => {
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetById,
                            &[old_value.index(), receiver.index(), property],
                            span,
                        )?;
                        self.logical_assignment(
                            operator,
                            old_value,
                            value,
                            span,
                            Some((Opcode::SetById, receiver.index(), property)),
                        )?
                    }
                };
                if !matches!(operator, HirAssignmentOperator::Logical(_)) {
                    self.emit(
                        Opcode::SetById,
                        &[receiver.index(), result.index(), property],
                        span,
                    )?;
                }
                Ok(result)
            }
            HirAssignmentTarget::ComputedMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.expression(property)?;
                let result = match operator {
                    HirAssignmentOperator::Assign => self.expression(value)?,
                    HirAssignmentOperator::Binary(operator) => {
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetByValue,
                            &[old_value.index(), receiver.index(), property.index()],
                            span,
                        )?;
                        let right = self.expression(value)?;
                        self.emit_binary(operator, old_value, right, span)?
                    }
                    HirAssignmentOperator::Logical(operator) => {
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetByValue,
                            &[old_value.index(), receiver.index(), property.index()],
                            span,
                        )?;
                        self.logical_assignment(
                            operator,
                            old_value,
                            value,
                            span,
                            Some((Opcode::SetByValue, receiver.index(), property.index())),
                        )?
                    }
                };
                if !matches!(operator, HirAssignmentOperator::Logical(_)) {
                    self.emit(
                        Opcode::SetByValue,
                        &[receiver.index(), result.index(), property.index()],
                        span,
                    )?;
                }
                Ok(result)
            }
        }
    }

    /// Preserves identifier-reference order while updating only an already resolved scope binding.
    fn scope_assignment(
        &mut self,
        operator: HirAssignmentOperator,
        target: &HirIdentifierReference,
        value: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        self.require_global_reference(target, span)?;
        let scope_name = self.resolved_global_binding(target, true)?;
        let result = match operator {
            HirAssignmentOperator::Assign => self.expression(value)?,
            HirAssignmentOperator::Binary(operator) => {
                let old_value = self.register()?;
                self.emit(Opcode::LoadScope, &[old_value.index(), scope_name], span)?;
                let right = self.expression(value)?;
                self.emit_binary(operator, old_value, right, span)?
            }
            HirAssignmentOperator::Logical(operator) => {
                let old_value = self.register()?;
                self.emit(Opcode::LoadScope, &[old_value.index(), scope_name], span)?;
                self.logical_assignment(operator, old_value, value, span, None)?
            }
        };
        self.emit(
            Opcode::StoreResolvedScope,
            &[result.index(), scope_name],
            span,
        )?;
        Ok(result)
    }

    /// Emits one supported binary operation over already evaluated values.
    fn emit_binary(
        &mut self,
        operator: HirBinaryOperator,
        left: RegisterId,
        right: RegisterId,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        if matches!(
            operator,
            HirBinaryOperator::Equal
                | HirBinaryOperator::NotEqual
                | HirBinaryOperator::StrictNotEqual
        ) {
            let equal = self.register()?;
            self.emit(
                Opcode::LooseEqual,
                &[equal.index(), left.index(), right.index()],
                span,
            )?;
            if operator != HirBinaryOperator::Equal {
                let destination = self.register()?;
                self.emit(Opcode::Not, &[destination.index(), equal.index()], span)?;
                return Ok(destination);
            }
            return Ok(equal);
        }
        let opcode = match operator {
            HirBinaryOperator::Add => Opcode::Add,
            HirBinaryOperator::Subtract => Opcode::Sub,
            HirBinaryOperator::Multiply => Opcode::Mul,
            HirBinaryOperator::Divide => Opcode::Div,
            HirBinaryOperator::Remainder => Opcode::Remainder,
            HirBinaryOperator::Exponentiate => Opcode::Exponentiate,
            HirBinaryOperator::BitwiseAnd => Opcode::BitwiseAnd,
            HirBinaryOperator::BitwiseOr => Opcode::BitwiseOr,
            HirBinaryOperator::BitwiseXor => Opcode::BitwiseXor,
            HirBinaryOperator::ShiftLeft => Opcode::ShiftLeft,
            HirBinaryOperator::ShiftRight => Opcode::ShiftRight,
            HirBinaryOperator::ShiftRightUnsigned => Opcode::ShiftRightUnsigned,
            HirBinaryOperator::StrictEqual => Opcode::StrictEqual,
            HirBinaryOperator::LessThan => Opcode::LessThan,
            HirBinaryOperator::GreaterThan => Opcode::GreaterThan,
            HirBinaryOperator::LessEqual => Opcode::LessEqual,
            HirBinaryOperator::GreaterEqual => Opcode::GreaterEqual,
            HirBinaryOperator::InstanceOf => Opcode::InstanceOf,
            HirBinaryOperator::In => Opcode::HasProperty,
            _ => {
                return Err(CompileError::UnsupportedSyntax {
                    source_name: self.source_name.clone(),
                    span,
                    syntax: "binary operator",
                });
            }
        };
        let destination = self.register()?;
        self.emit(
            opcode,
            &[destination.index(), left.index(), right.index()],
            span,
        )?;
        Ok(destination)
    }

    /// Reads one update reference once and preserves the prefix/postfix result distinction.
    fn update_expression(
        &mut self,
        operator: HirUpdateOperator,
        prefix: bool,
        target: &HirAssignmentTarget,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let opcode = match operator {
            HirUpdateOperator::Increment => HirBinaryOperator::Add,
            HirUpdateOperator::Decrement => HirBinaryOperator::Subtract,
        };
        match target {
            HirAssignmentTarget::Identifier(target) => {
                if let Some(binding) = self.local_reference(target).cloned() {
                    if !binding.mutable {
                        return Err(self.unsupported(span, "update of immutable local"));
                    }
                    let old = self.snapshot_local(&binding, span)?;
                    let one = self.load_immediate(1, span)?;
                    let updated = self.emit_binary(opcode, old, one, span)?;
                    self.write_local(&binding, updated, span)?;
                    return Ok(if prefix { updated } else { old });
                }
                if let Some(binding) = self.captured_reference(target)? {
                    if !binding.mutable {
                        return Err(self.unsupported(span, "update of immutable capture"));
                    }
                    let old = self.snapshot_local(&binding, span)?;
                    let one = self.load_immediate(1, span)?;
                    let updated = self.emit_binary(opcode, old, one, span)?;
                    self.write_local(&binding, updated, span)?;
                    return Ok(if prefix { updated } else { old });
                }
                self.scope_update(opcode, prefix, target, span)
            }
            HirAssignmentTarget::StaticMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.scope_name(property)?;
                let old = self.register()?;
                self.emit(
                    Opcode::GetById,
                    &[old.index(), receiver.index(), property],
                    span,
                )?;
                let result = if prefix {
                    None
                } else {
                    let snapshot = self.register()?;
                    self.emit(Opcode::Move, &[snapshot.index(), old.index()], span)?;
                    Some(snapshot)
                };
                let one = self.load_immediate(1, span)?;
                let updated = self.emit_binary(opcode, old, one, span)?;
                self.emit(
                    Opcode::SetById,
                    &[receiver.index(), updated.index(), property],
                    span,
                )?;
                Ok(result.unwrap_or(updated))
            }
            HirAssignmentTarget::ComputedMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.expression(property)?;
                let old = self.register()?;
                self.emit(
                    Opcode::GetByValue,
                    &[old.index(), receiver.index(), property.index()],
                    span,
                )?;
                let result = if prefix {
                    None
                } else {
                    let snapshot = self.register()?;
                    self.emit(Opcode::Move, &[snapshot.index(), old.index()], span)?;
                    Some(snapshot)
                };
                let one = self.load_immediate(1, span)?;
                let updated = self.emit_binary(opcode, old, one, span)?;
                self.emit(
                    Opcode::SetByValue,
                    &[receiver.index(), updated.index(), property.index()],
                    span,
                )?;
                Ok(result.unwrap_or(updated))
            }
        }
    }

    /// Loads, snapshots, updates, and stores one dynamically resolved identifier exactly once.
    fn scope_update(
        &mut self,
        opcode: HirBinaryOperator,
        prefix: bool,
        target: &HirIdentifierReference,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        self.require_global_reference(target, span)?;
        let scope_name = self.resolved_global_binding(target, true)?;
        let old = self.register()?;
        self.emit(Opcode::LoadScope, &[old.index(), scope_name], span)?;
        let result = if prefix {
            None
        } else {
            let snapshot = self.register()?;
            self.emit(Opcode::Move, &[snapshot.index(), old.index()], span)?;
            Some(snapshot)
        };
        let one = self.load_immediate(1, span)?;
        let updated = self.emit_binary(opcode, old, one, span)?;
        self.emit(
            Opcode::StoreResolvedScope,
            &[updated.index(), scope_name],
            span,
        )?;
        Ok(result.unwrap_or(updated))
    }

    /// Preserves the left operand value and evaluates the right operand only when required.
    fn logical(
        &mut self,
        operator: HirLogicalOperator,
        left: &HirExpression,
        right: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let left = self.expression(left)?;
        let destination = self.register()?;
        self.emit(Opcode::Move, &[destination.index(), left.index()], span)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        let source_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        match operator {
            HirLogicalOperator::And => self.builder.emit_jump_if_false(left, end, source_span),
            HirLogicalOperator::Or => self.builder.emit_jump_if_true(left, end, source_span),
            HirLogicalOperator::Coalesce => {
                self.builder
                    .emit_jump_if_not_nullish(left, end, source_span)
            }
        }
        .map_err(CompileError::Builder)?;
        let right = self.expression(right)?;
        self.emit(Opcode::Move, &[destination.index(), right.index()], span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        Ok(destination)
    }

    /// Preserves an assignment target's old value and evaluates RHS only when its logical test fails.
    fn logical_assignment(
        &mut self,
        operator: HirLogicalOperator,
        old: RegisterId,
        value: &HirExpression,
        span: SourceSpan,
        store: Option<(Opcode, u32, u32)>,
    ) -> Result<RegisterId, CompileError> {
        let destination = self.register()?;
        self.emit(Opcode::Move, &[destination.index(), old.index()], span)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        let source_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        match operator {
            HirLogicalOperator::And => self.builder.emit_jump_if_false(old, end, source_span),
            HirLogicalOperator::Or => self.builder.emit_jump_if_true(old, end, source_span),
            HirLogicalOperator::Coalesce => {
                self.builder.emit_jump_if_not_nullish(old, end, source_span)
            }
        }
        .map_err(CompileError::Builder)?;
        let right = self.expression(value)?;
        self.emit(Opcode::Move, &[destination.index(), right.index()], span)?;
        if let Some((opcode, receiver, property)) = store {
            self.emit(opcode, &[receiver, destination.index(), property], span)?;
        }
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        Ok(destination)
    }

    /// Evaluates callee/arguments in source order and copies them into the verified contiguous call window.
    fn call_expression(
        &mut self,
        callee: &HirExpression,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        if let HirExpressionKind::StaticMember { object, property } = &callee.kind {
            return self.method_call_expression(object, property, arguments, span);
        }
        let callee_value = self.expression(callee)?;
        let call_base = self.register()?;
        self.emit(
            Opcode::Move,
            &[call_base.index(), callee_value.index()],
            span,
        )?;
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::Call,
            &[destination.index(), call_base.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Evaluates constructor and arguments once before emitting one verified construct window.
    fn construct_expression(
        &mut self,
        callee: &HirExpression,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let callee_value = self.expression(callee)?;
        let call_base = self.register()?;
        self.emit(
            Opcode::Move,
            &[call_base.index(), callee_value.index()],
            span,
        )?;
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::Construct,
            &[destination.index(), call_base.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Materializes receiver/callee/arguments once in one verified contiguous method-call window.
    fn method_call_expression(
        &mut self,
        object: &HirExpression,
        property: &std::sync::Arc<str>,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let receiver_value = self.expression(object)?;
        let call_base = self.register()?;
        self.emit(
            Opcode::Move,
            &[call_base.index(), receiver_value.index()],
            span,
        )?;
        let callee_slot = self.register()?;
        let property = self.scope_name(property)?;
        self.emit(
            Opcode::GetById,
            &[callee_slot.index(), call_base.index(), property],
            span,
        )?;
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::CallWithReceiver,
            &[destination.index(), call_base.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Emits both arms into one result register and resolves their labels before bytecode becomes immutable.
    fn conditional(
        &mut self,
        test: &HirExpression,
        consequent: &HirExpression,
        alternate: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let test = self.expression(test)?;
        let alternate_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let destination = self.register()?;
        let source_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        self.builder
            .emit_jump_if_false(test, alternate_label, source_span)
            .map_err(CompileError::Builder)?;
        let consequent = self.expression(consequent)?;
        self.emit(
            Opcode::Move,
            &[destination.index(), consequent.index()],
            span,
        )?;
        self.builder
            .emit_jump(end_label, source_span)
            .map_err(CompileError::Builder)?;
        self.builder
            .bind_label(alternate_label)
            .map_err(CompileError::Builder)?;
        let alternate = self.expression(alternate)?;
        self.emit(
            Opcode::Move,
            &[destination.index(), alternate.index()],
            span,
        )?;
        self.builder
            .bind_label(end_label)
            .map_err(CompileError::Builder)?;
        Ok(destination)
    }

    /// Lowers one declaration list in source order so initializers can use preceding local bindings.
    fn variable_declaration(
        &mut self,
        declaration: &HirVariableDeclaration,
    ) -> Result<(), CompileError> {
        if declaration.kind == HirVariableDeclarationKind::Var {
            return self.var_initializers(declaration);
        }
        if !matches!(
            declaration.kind,
            HirVariableDeclarationKind::Let | HirVariableDeclarationKind::Const
        ) {
            return Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: declaration
                    .declarators
                    .first()
                    .map_or(SourceSpan { start: 0, end: 0 }, |declarator| {
                        declarator.span
                    }),
                syntax: "variable declaration kind",
            });
        }
        for declarator in declaration.declarators.iter() {
            let register = match declarator.initializer.as_ref() {
                Some(initializer) => self.expression(initializer)?,
                None if declaration.kind == HirVariableDeclarationKind::Let => {
                    self.load_undefined(declarator.span)?
                }
                None => {
                    return Err(CompileError::UnsupportedSyntax {
                        source_name: self.source_name.clone(),
                        span: declarator.span,
                        syntax: "variable declaration without initializer",
                    });
                }
            };
            if self.script_scope && declarator.binding.scope == self.root_scope {
                let lexical = self
                    .environments
                    .global_lexical(declarator.binding.id)
                    .ok_or(CompileError::BindingOverflow)?;
                let scope_name = self.global_lexical_binding(lexical)?;
                self.emit(
                    Opcode::InitializeGlobalLexical,
                    &[register.index(), scope_name],
                    declarator.span,
                )?;
                continue;
            }
            self.add_local(
                &declarator.binding,
                Some(register),
                declaration.kind == HirVariableDeclarationKind::Let,
            )?;
        }
        Ok(())
    }

    /// Executes var initializers at their source position against bindings instantiated at entry.
    fn var_initializers(
        &mut self,
        declaration: &HirVariableDeclaration,
    ) -> Result<(), CompileError> {
        for declarator in declaration.declarators.iter() {
            let Some(initializer) = declarator.initializer.as_ref() else {
                continue;
            };
            let value = self.expression(initializer)?;
            if let Some(binding) = self.local_by_id(declarator.binding.id).cloned() {
                self.write_local(&binding, value, declarator.span)?;
            } else if self.script_scope {
                let scope_name = self.global_binding(&declarator.binding.name, true)?;
                self.emit(
                    Opcode::StoreScope,
                    &[value.index(), scope_name],
                    declarator.span,
                )?;
            } else {
                return Err(self.unsupported(declarator.span, "uninstantiated var binding"));
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn local_by_id(&self, id: BindingId) -> Option<&LocalBinding> {
        self.locals.iter().rev().find(|binding| binding.id == id)
    }

    #[inline(always)]
    fn local_reference(&self, reference: &HirIdentifierReference) -> Option<&LocalBinding> {
        reference.binding.and_then(|id| self.local_by_id(id))
    }

    fn require_global_reference(
        &self,
        reference: &HirIdentifierReference,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        if reference.binding.is_some() && reference.binding_scope != Some(self.root_scope) {
            return Err(self.unsupported(span, "captured binding requires environment storage"));
        }
        Ok(())
    }

    fn load_immediate(&mut self, value: u32, span: SourceSpan) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        self.emit(Opcode::LoadImmediate, &[register.index(), value], span)?;
        Ok(register)
    }

    fn load_undefined(&mut self, span: SourceSpan) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        self.emit(Opcode::LoadUndefined, &[register.index()], span)?;
        Ok(register)
    }

    fn load_null(&mut self, span: SourceSpan) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        self.emit(Opcode::LoadNull, &[register.index()], span)?;
        Ok(register)
    }

    fn load_boolean(&mut self, value: bool, span: SourceSpan) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        let opcode = if value {
            Opcode::LoadTrue
        } else {
            Opcode::LoadFalse
        };
        self.emit(opcode, &[register.index()], span)?;
        Ok(register)
    }

    fn register(&mut self) -> Result<RegisterId, CompileError> {
        let register = RegisterId::new(self.next_register);
        self.next_register = self
            .next_register
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        Ok(register)
    }

    fn add_binding_plan(&mut self, entry: BindingPlanEntry) -> Result<u32, CompileError> {
        if let Some(index) = self
            .binding_plan
            .iter()
            .position(|existing| existing == &entry)
        {
            return u32::try_from(index).map_err(|_| CompileError::BindingOverflow);
        }
        let index =
            u32::try_from(self.binding_plan.len()).map_err(|_| CompileError::BindingOverflow)?;
        self.binding_plan.push(entry);
        Ok(index)
    }

    /// Materializes one ancestor capture as this function's immutable binding-plan entry.
    fn captured_reference(
        &mut self,
        reference: &HirIdentifierReference,
    ) -> Result<Option<LocalBinding>, CompileError> {
        let (Some(function_scope), Some(binding)) = (self.function_scope, reference.binding) else {
            return Ok(None);
        };
        let Some((depth, slot)) = self.environments.reference_slot(function_scope, binding) else {
            return Ok(None);
        };
        self.add_binding_plan(BindingPlanEntry {
            name: slot.name.clone(),
            location: BindingLocation::Environment {
                depth,
                slot: slot.slot,
            },
            mutable: slot.mutable,
        })?;
        Ok(Some(LocalBinding {
            id: binding,
            storage: LocalStorage::Environment {
                depth,
                slot: slot.slot,
            },
            mutable: slot.mutable,
        }))
    }

    fn read_local(
        &mut self,
        binding: &LocalBinding,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        match binding.storage {
            LocalStorage::Register(register) => Ok(register),
            LocalStorage::Environment { depth, slot } => {
                let destination = self.register()?;
                self.emit(
                    Opcode::LoadEnvironment,
                    &[destination.index(), depth, slot],
                    span,
                )?;
                Ok(destination)
            }
        }
    }

    fn snapshot_local(
        &mut self,
        binding: &LocalBinding,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let value = self.read_local(binding, span)?;
        if matches!(binding.storage, LocalStorage::Environment { .. }) {
            return Ok(value);
        }
        let snapshot = self.register()?;
        self.emit(Opcode::Move, &[snapshot.index(), value.index()], span)?;
        Ok(snapshot)
    }

    fn write_local(
        &mut self,
        binding: &LocalBinding,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        match binding.storage {
            LocalStorage::Register(register) => {
                self.emit(Opcode::Move, &[register.index(), value.index()], span)
            }
            LocalStorage::Environment { depth, slot } => self.emit(
                Opcode::StoreEnvironment,
                &[value.index(), depth, slot],
                span,
            ),
        }
    }

    /// Publishes one local binding and initializes promoted parameters through verified bytecode.
    fn add_local(
        &mut self,
        binding: &crate::HirBinding,
        register: Option<RegisterId>,
        mutable: bool,
    ) -> Result<(), CompileError> {
        let storage = if let Some(function_scope) = self.function_scope
            && let Some(slot) = self.environments.local_slot(function_scope, binding.id)
        {
            self.add_binding_plan(BindingPlanEntry {
                name: slot.name.clone(),
                location: BindingLocation::Environment {
                    depth: 0,
                    slot: slot.slot,
                },
                mutable: slot.mutable,
            })?;
            if let Some(register) = register {
                self.emit(
                    Opcode::StoreEnvironment,
                    &[register.index(), 0, slot.slot],
                    binding.span,
                )?;
            }
            LocalStorage::Environment {
                depth: 0,
                slot: slot.slot,
            }
        } else {
            let register = register.ok_or(CompileError::RegisterOverflow)?;
            self.add_binding_plan(BindingPlanEntry {
                name: binding.name.clone(),
                location: BindingLocation::FrameRegister(register),
                mutable,
            })?;
            LocalStorage::Register(register)
        };
        self.locals.push(LocalBinding {
            id: binding.id,
            storage,
            mutable,
        });
        Ok(())
    }

    /// Records one global-property binding once while returning its shared module name index.
    fn global_binding(
        &mut self,
        name: &std::sync::Arc<str>,
        mutable: bool,
    ) -> Result<u32, CompileError> {
        let scope_name = self.scope_name(name)?;
        let entry = BindingPlanEntry {
            name: name.clone(),
            location: BindingLocation::GlobalProperty,
            mutable,
        };
        if !self.binding_plan.contains(&entry) {
            self.binding_plan.push(entry);
        }
        Ok(scope_name)
    }

    fn global_lexical_binding(&mut self, lexical: &GlobalLexicalPlan) -> Result<u32, CompileError> {
        let scope_name = self.scope_name(&lexical.name)?;
        self.add_binding_plan(BindingPlanEntry {
            name: lexical.name.clone(),
            location: BindingLocation::GlobalLexical,
            mutable: lexical.mutable,
        })?;
        Ok(scope_name)
    }

    fn resolved_global_binding(
        &mut self,
        reference: &HirIdentifierReference,
        mutable: bool,
    ) -> Result<u32, CompileError> {
        if let Some(lexical) = reference
            .binding
            .and_then(|binding| self.environments.global_lexical(binding))
        {
            return self.global_lexical_binding(lexical);
        }
        self.global_binding(&reference.name, mutable)
    }

    /// Returns a module-stable scope-name index while retaining only one owned copy per spelling.
    fn scope_name(&mut self, name: &std::sync::Arc<str>) -> Result<u32, CompileError> {
        if let Some(index) = self
            .scope_names
            .iter()
            .position(|existing| existing.as_ref() == name.as_ref())
        {
            return u32::try_from(index).map_err(|_| CompileError::BindingOverflow);
        }
        let index =
            u32::try_from(self.scope_names.len()).map_err(|_| CompileError::BindingOverflow)?;
        self.scope_names.push(name.clone());
        Ok(index)
    }
}

fn hir_instruction_count(hir: &HirProgram) -> Result<usize, CompileError> {
    statements_instruction_count(hir.statements())
}

/// Collects function/script-scoped var names once with a source-derived no-growth capacity.
fn var_declared_bindings(
    statements: &[HirStatement],
) -> Result<Vec<crate::HirBinding>, CompileError> {
    let mut bindings = Vec::with_capacity(statements_binding_count(statements)?);
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
        if bindings
            .iter()
            .all(|binding| binding.id != declarator.binding.id)
        {
            bindings.push(declarator.binding.clone());
        }
    }
}

/// Adds every owned try child with the same checked collection-specific counter.
fn try_children_count(
    block: &[HirStatement],
    handler: Option<&HirCatchClause>,
    finalizer: Option<&[HirStatement]>,
    collection: &'static str,
    counter: fn(&[HirStatement]) -> Result<usize, CompileError>,
) -> Result<usize, CompileError> {
    let mut count = counter(block)?;
    if let Some(handler) = handler {
        count = checked_count_add(count, counter(&handler.body)?, collection)?;
    }
    if let Some(finalizer) = finalizer {
        count = checked_count_add(count, counter(finalizer)?, collection)?;
    }
    Ok(count)
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

/// Counts handler records exactly, including nested ranges in every try arm.
fn statements_handler_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_handler_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_handler_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_handler_count(core::slice::from_ref(alternate))?,
                        "exception handlers",
                    )?;
                }
                nested
            }
            HirStatementKind::For { body, .. } | HirStatementKind::Loop { body, .. } => {
                statements_handler_count(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => {
                let mut nested = 0;
                for case in cases.iter() {
                    nested = checked_count_add(
                        nested,
                        statements_handler_count(&case.consequent)?,
                        "exception handlers",
                    )?;
                }
                nested
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let nested = try_children_count(
                    block,
                    handler.as_ref(),
                    finalizer.as_deref(),
                    "exception handlers",
                    statements_handler_count,
                )?;
                checked_count_add(nested, usize::from(handler.is_some()), "exception handlers")?
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, nested, "exception handlers")?;
    }
    Ok(count)
}

/// Computes exact simultaneous catch-range depth rather than total handler count.
fn statements_handler_depth(statements: &[HirStatement]) -> Result<u32, CompileError> {
    let mut depth = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_handler_depth(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let consequent = statements_handler_depth(core::slice::from_ref(consequent))?;
                let alternate = alternate
                    .as_ref()
                    .map(|statement| statements_handler_depth(core::slice::from_ref(statement)))
                    .transpose()?
                    .unwrap_or(0);
                consequent.max(alternate)
            }
            HirStatementKind::For { body, .. } | HirStatementKind::Loop { body, .. } => {
                statements_handler_depth(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => {
                let mut nested = 0;
                for case in cases.iter() {
                    nested = nested.max(statements_handler_depth(&case.consequent)?);
                }
                nested
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let block = statements_handler_depth(block)?
                    .checked_add(u32::from(handler.is_some()))
                    .ok_or(CompileError::LoweringCapacityOverflow {
                        collection: "exception handler depth",
                    })?;
                let handler = handler
                    .as_ref()
                    .map(|handler| statements_handler_depth(&handler.body))
                    .transpose()?
                    .unwrap_or(0);
                let finalizer = finalizer
                    .as_ref()
                    .map(|statements| statements_handler_depth(statements))
                    .transpose()?
                    .unwrap_or(0);
                block.max(handler).max(finalizer)
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        depth = depth.max(nested);
    }
    Ok(depth)
}

/// Computes a checked upper bound for module scope-name interning before HIR lowering starts.
fn hir_scope_name_capacity(hir: &HirProgram) -> Result<usize, CompileError> {
    let mut count = checked_count_add(
        statements_scope_name_count(hir.statements())?,
        var_declared_bindings(hir.statements())?.len(),
        "scope names",
    )?;
    for function in hir.functions() {
        count = checked_count_add(
            count,
            statements_scope_name_count(&function.body)?,
            "scope names",
        )?;
        count = checked_count_add(
            count,
            parameter_scope_name_count(&function.parameter_initializers)?,
            "scope names",
        )?;
    }
    Ok(count)
}

/// Counts identifier references and published top-level function names across structured statements.
fn statements_scope_name_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_scope_name_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                let mut nested = 0;
                for initializer in declaration
                    .declarators
                    .iter()
                    .filter_map(|declarator| declarator.initializer.as_ref())
                {
                    nested = checked_count_add(
                        nested,
                        expression_scope_name_count(initializer)?,
                        "scope names",
                    )?;
                }
                nested
            }
            HirStatementKind::FunctionDeclaration(_) => 1,
            HirStatementKind::Block(statements) => statements_scope_name_count(statements)?,
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                let mut nested = expression_scope_name_count(test)?;
                nested = checked_count_add(
                    nested,
                    statements_scope_name_count(core::slice::from_ref(consequent))?,
                    "scope names",
                )?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_scope_name_count(core::slice::from_ref(alternate))?,
                        "scope names",
                    )?;
                }
                nested
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => for_scope_name_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
            HirStatementKind::Loop { test, body, .. } => checked_count_add(
                expression_scope_name_count(test)?,
                statements_scope_name_count(core::slice::from_ref(body))?,
                "scope names",
            )?,
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => switch_scope_name_count(discriminant, cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "scope names",
                statements_scope_name_count,
            )?,
            HirStatementKind::Return(argument) => argument
                .as_ref()
                .map(expression_scope_name_count)
                .transpose()?
                .unwrap_or(0),
            HirStatementKind::Throw(argument) => expression_scope_name_count(argument)?,
            HirStatementKind::Break | HirStatementKind::Continue | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "scope names")?;
    }
    Ok(count)
}

/// Counts every for-loop expression and body reference without flattening evaluation order.
fn for_scope_name_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = match initializer {
        Some(HirForInitializer::Variable(declaration)) => {
            declaration_scope_name_count(declaration)?
        }
        Some(HirForInitializer::Expression(expression)) => expression_scope_name_count(expression)?,
        None => 0,
    };
    for expression in [test, update].into_iter().flatten() {
        count = checked_count_add(
            count,
            expression_scope_name_count(expression)?,
            "scope names",
        )?;
    }
    checked_count_add(
        count,
        statements_scope_name_count(core::slice::from_ref(body))?,
        "scope names",
    )
}

/// Counts identifier references in every declaration initializer.
fn declaration_scope_name_count(
    declaration: &HirVariableDeclaration,
) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in declaration
        .declarators
        .iter()
        .filter_map(|declarator| declarator.initializer.as_ref())
    {
        count = checked_count_add(
            count,
            expression_scope_name_count(initializer)?,
            "scope names",
        )?;
    }
    Ok(count)
}

/// Counts discriminant, case-test, and clause-body identifiers without flattening case order.
fn switch_scope_name_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = expression_scope_name_count(discriminant)?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count = checked_count_add(count, expression_scope_name_count(test)?, "scope names")?;
        }
        count = checked_count_add(
            count,
            statements_scope_name_count(&case.consequent)?,
            "scope names",
        )?;
    }
    Ok(count)
}

/// Counts every identifier occurrence in one expression as a conservative scope-name upper bound.
fn expression_scope_name_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Identifier(_) => Ok(1),
        HirExpressionKind::Object(properties) => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    expression_scope_name_count(&property.value)?,
                    "scope names",
                )?;
                if matches!(property.key, HirObjectPropertyKey::Static(_)) {
                    count = checked_count_add(count, 1, "scope names")?;
                }
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count =
                        checked_count_add(count, expression_scope_name_count(key)?, "scope names")?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::StaticMember { object, .. } => {
            checked_count_add(expression_scope_name_count(object)?, 1, "scope names")
        }
        HirExpressionKind::ComputedMember { object, property } => checked_count_add(
            expression_scope_name_count(object)?,
            expression_scope_name_count(property)?,
            "scope names",
        ),
        HirExpressionKind::Unary { argument, .. } => expression_scope_name_count(argument),
        HirExpressionKind::Binary { left, right, .. } => checked_count_add(
            expression_scope_name_count(left)?,
            expression_scope_name_count(right)?,
            "scope names",
        ),
        HirExpressionKind::Logical { left, right, .. } => checked_count_add(
            expression_scope_name_count(left)?,
            expression_scope_name_count(right)?,
            "scope names",
        ),
        HirExpressionKind::Assignment { target, value, .. } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 1,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    checked_count_add(expression_scope_name_count(object)?, 1, "scope names")?
                }
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_scope_name_count(object)?,
                    expression_scope_name_count(property)?,
                    "scope names",
                )?,
            };
            checked_count_add(target, expression_scope_name_count(value)?, "scope names")
        }
        HirExpressionKind::Update { target, .. } => match target {
            HirAssignmentTarget::Identifier(_) => Ok(1),
            HirAssignmentTarget::StaticMember { object, .. } => {
                checked_count_add(expression_scope_name_count(object)?, 1, "scope names")
            }
            HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                expression_scope_name_count(object)?,
                expression_scope_name_count(property)?,
                "scope names",
            ),
        },
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => checked_count_add(
            expression_scope_name_count(test)?,
            checked_count_add(
                expression_scope_name_count(consequent)?,
                expression_scope_name_count(alternate)?,
                "scope names",
            )?,
            "scope names",
        ),
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_scope_name_count(callee)?;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_scope_name_count(argument)?,
                    "scope names",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::Number(_)
        | HirExpressionKind::String(_)
        | HirExpressionKind::Boolean(_)
        | HirExpressionKind::Null
        | HirExpressionKind::Function(_)
        | HirExpressionKind::This
        | HirExpressionKind::NewTarget => Ok(0),
    }
}

/// Computes a checked instruction upper bound across nested structured statements.
fn statements_instruction_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_instruction_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                declaration_instruction_count(declaration)?
            }
            HirStatementKind::FunctionDeclaration(_) => 2,
            HirStatementKind::Return(argument) => argument
                .as_ref()
                .map(expression_instruction_count)
                .transpose()?
                .unwrap_or(1)
                .checked_add(1)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "bytecode instructions",
                })?,
            HirStatementKind::Throw(argument) => checked_count_add(
                expression_instruction_count(argument)?,
                1,
                "bytecode instructions",
            )?,
            HirStatementKind::Block(statements) => statements_instruction_count(statements)?,
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                let mut count = expression_instruction_count(test)?;
                count = checked_count_add(count, 2, "bytecode instructions")?;
                count = checked_count_add(
                    count,
                    statements_instruction_count(core::slice::from_ref(consequent))?,
                    "bytecode instructions",
                )?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_instruction_count(core::slice::from_ref(alternate))?,
                        "bytecode instructions",
                    )?;
                }
                count
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => for_instruction_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
            HirStatementKind::Loop {
                test,
                body,
                test_first,
            } => {
                let mut nested = expression_instruction_count(test)?;
                nested = checked_count_add(
                    nested,
                    statements_instruction_count(core::slice::from_ref(body))?,
                    "bytecode instructions",
                )?;
                checked_count_add(
                    nested,
                    1 + usize::from(*test_first),
                    "bytecode instructions",
                )?
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => switch_instruction_count(discriminant, cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let nested = try_children_count(
                    block,
                    handler.as_ref(),
                    finalizer.as_deref(),
                    "bytecode instructions",
                    statements_instruction_count,
                )?;
                checked_count_add(
                    nested,
                    if handler.is_some() { 3 } else { 0 },
                    "bytecode instructions",
                )?
            }
            HirStatementKind::Break | HirStatementKind::Continue => 1,
            HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode instructions")?;
    }
    Ok(count)
}

/// Counts classic for-loop evaluation plus its conditional and back-edge instructions exactly.
fn for_instruction_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = match initializer {
        Some(HirForInitializer::Variable(declaration)) => {
            declaration_instruction_count(declaration)?
        }
        Some(HirForInitializer::Expression(expression)) => {
            expression_instruction_count(expression)?
        }
        None => 0,
    };
    if let Some(test) = test {
        count = checked_count_add(
            count,
            expression_instruction_count(test)?,
            "bytecode instructions",
        )?;
        count = checked_count_add(count, 1, "bytecode instructions")?;
    }
    if let Some(update) = update {
        count = checked_count_add(
            count,
            expression_instruction_count(update)?,
            "bytecode instructions",
        )?;
    }
    count = checked_count_add(
        count,
        statements_instruction_count(core::slice::from_ref(body))?,
        "bytecode instructions",
    )?;
    checked_count_add(count, 1, "bytecode instructions")
}

/// Includes dispatch comparisons, conditional branches, fallback jump, and every clause body.
fn switch_instruction_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = checked_count_add(
        expression_instruction_count(discriminant)?,
        2,
        "bytecode instructions",
    )?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count = checked_count_add(
                count,
                expression_instruction_count(test)?,
                "bytecode instructions",
            )?;
            count = checked_count_add(count, 2, "bytecode instructions")?;
        }
        count = checked_count_add(
            count,
            statements_instruction_count(&case.consequent)?,
            "bytecode instructions",
        )?;
    }
    Ok(count)
}

fn declaration_instruction_count(
    declaration: &HirVariableDeclaration,
) -> Result<usize, CompileError> {
    let mut count = 0;
    for declarator in declaration.declarators.iter() {
        let initializer_count = match declarator.initializer.as_ref() {
            Some(initializer) => {
                let count = expression_instruction_count(initializer)?;
                if declaration.kind == HirVariableDeclarationKind::Var {
                    checked_count_add(count, 1, "bytecode instructions")?
                } else {
                    count
                }
            }
            None if declaration.kind == HirVariableDeclarationKind::Var => 0,
            None => 1,
        };
        count = checked_count_add(count, initializer_count, "bytecode instructions")?;
    }
    Ok(count)
}

fn expression_instruction_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Object(properties) => {
            let mut count = 1;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    expression_instruction_count(&property.value)?,
                    "bytecode instructions",
                )?;
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count = checked_count_add(
                        count,
                        expression_instruction_count(key)?,
                        "bytecode instructions",
                    )?;
                }
                count = checked_count_add(count, 1, "bytecode instructions")?;
            }
            Ok(count)
        }
        HirExpressionKind::Binary {
            operator,
            left,
            right,
        } => {
            let operands = checked_count_add(
                expression_instruction_count(left)?,
                expression_instruction_count(right)?,
                "bytecode instructions",
            )?;
            let own = if matches!(operator, HirBinaryOperator::StrictNotEqual) {
                2
            } else {
                1
            };
            checked_count_add(own, operands, "bytecode instructions")
        }
        HirExpressionKind::Logical { left, right, .. } => {
            let operands = checked_count_add(
                expression_instruction_count(left)?,
                expression_instruction_count(right)?,
                "bytecode instructions",
            )?;
            checked_count_add(operands, 3, "bytecode instructions")
        }
        HirExpressionKind::StaticMember { object, .. } => checked_count_add(
            expression_instruction_count(object)?,
            1,
            "bytecode instructions",
        ),
        HirExpressionKind::ComputedMember { object, property } => {
            let operands = checked_count_add(
                expression_instruction_count(object)?,
                expression_instruction_count(property)?,
                "bytecode instructions",
            )?;
            checked_count_add(operands, 1, "bytecode instructions")
        }
        HirExpressionKind::Assignment {
            operator,
            target,
            value,
        } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    expression_instruction_count(object)?
                }
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_instruction_count(object)?,
                    expression_instruction_count(property)?,
                    "bytecode instructions",
                )?,
            };
            let operands = checked_count_add(
                target,
                expression_instruction_count(value)?,
                "bytecode instructions",
            )?;
            let own_instructions = match (operator, target) {
                (HirAssignmentOperator::Assign, _) => 1,
                (HirAssignmentOperator::Binary(_), _) => 3,
                (HirAssignmentOperator::Logical(_), _) => 5,
            };
            checked_count_add(operands, own_instructions, "bytecode instructions")
        }
        HirExpressionKind::Update { prefix, target, .. } => {
            let identifier_target = matches!(target, HirAssignmentTarget::Identifier(_));
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => checked_count_add(
                    expression_instruction_count(object)?,
                    1,
                    "bytecode instructions",
                )?,
                HirAssignmentTarget::ComputedMember { object, property } => {
                    let operands = checked_count_add(
                        expression_instruction_count(object)?,
                        expression_instruction_count(property)?,
                        "bytecode instructions",
                    )?;
                    checked_count_add(operands, 1, "bytecode instructions")?
                }
            };
            let own = match (identifier_target, *prefix) {
                (true, true) => 4,
                (true, false) => 5,
                (false, true) => 3,
                (false, false) => 4,
            };
            checked_count_add(target, own, "bytecode instructions")
        }
        HirExpressionKind::Unary { argument, .. } => checked_count_add(
            expression_instruction_count(argument)?,
            1,
            "bytecode instructions",
        ),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => {
            let arms = checked_count_add(
                expression_instruction_count(consequent)?,
                expression_instruction_count(alternate)?,
                "bytecode instructions",
            )?;
            let branches = checked_count_add(arms, 4, "bytecode instructions")?;
            checked_count_add(
                expression_instruction_count(test)?,
                branches,
                "bytecode instructions",
            )
        }
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_instruction_count(callee)?;
            count = checked_count_add(count, 2, "bytecode instructions")?;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_instruction_count(argument)?,
                    "bytecode instructions",
                )?;
                count = checked_count_add(count, 1, "bytecode instructions")?;
            }
            Ok(count)
        }
        _ => Ok(1),
    }
}

fn hir_literal_count(hir: &HirProgram) -> Result<usize, CompileError> {
    let mut count = statements_literal_count(hir.statements())?;
    for function in hir.functions() {
        count = checked_count_add(
            count,
            statements_literal_count(&function.body)?,
            "bytecode constants",
        )?;
        count = checked_count_add(
            count,
            parameter_literal_count(&function.parameter_initializers)?,
            "bytecode constants",
        )?;
    }
    Ok(count)
}

/// Counts the prologue and expression instructions needed by default parameter initializers.
fn parameter_initializer_instruction_count(
    initializers: &[Option<HirExpression>],
) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in initializers.iter().flatten() {
        count = checked_count_add(
            count,
            expression_instruction_count(initializer)?,
            "function parameter instructions",
        )?;
        count = checked_count_add(count, 4, "function parameter instructions")?;
    }
    Ok(count)
}

fn parameter_scope_name_count(
    initializers: &[Option<HirExpression>],
) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in initializers.iter().flatten() {
        count = checked_count_add(
            count,
            expression_scope_name_count(initializer)?,
            "scope names",
        )?;
    }
    Ok(count)
}

fn parameter_literal_count(initializers: &[Option<HirExpression>]) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in initializers.iter().flatten() {
        count = checked_count_add(
            count,
            expression_literal_count(initializer)?,
            "bytecode constants",
        )?;
    }
    Ok(count)
}

/// Counts literal constants recursively before the module-wide pool is allocated.
fn statements_literal_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_literal_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                declaration_literal_count(declaration)?
            }
            HirStatementKind::Return(argument) => argument
                .as_ref()
                .map(expression_literal_count)
                .transpose()?
                .unwrap_or(0),
            HirStatementKind::Throw(argument) => expression_literal_count(argument)?,
            HirStatementKind::Block(statements) => statements_literal_count(statements)?,
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                let mut count = expression_literal_count(test)?;
                count = checked_count_add(
                    count,
                    statements_literal_count(core::slice::from_ref(consequent))?,
                    "bytecode constants",
                )?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_literal_count(core::slice::from_ref(alternate))?,
                        "bytecode constants",
                    )?;
                }
                count
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => for_literal_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
            HirStatementKind::Loop { test, body, .. } => checked_count_add(
                expression_literal_count(test)?,
                statements_literal_count(core::slice::from_ref(body))?,
                "bytecode constants",
            )?,
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => switch_literal_count(discriminant, cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "bytecode constants",
                statements_literal_count,
            )?,
            HirStatementKind::FunctionDeclaration(_) => 0,
            HirStatementKind::Break | HirStatementKind::Continue | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode constants")?;
    }
    Ok(count)
}

/// Counts constants in each loop component without counting the synthetic update value one.
fn for_literal_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = match initializer {
        Some(HirForInitializer::Variable(declaration)) => declaration_literal_count(declaration)?,
        Some(HirForInitializer::Expression(expression)) => expression_literal_count(expression)?,
        None => 0,
    };
    for expression in [test, update].into_iter().flatten() {
        count = checked_count_add(
            count,
            expression_literal_count(expression)?,
            "bytecode constants",
        )?;
    }
    checked_count_add(
        count,
        statements_literal_count(core::slice::from_ref(body))?,
        "bytecode constants",
    )
}

/// Counts constants in both dispatch expressions and source-ordered clause bodies.
fn switch_literal_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = expression_literal_count(discriminant)?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count =
                checked_count_add(count, expression_literal_count(test)?, "bytecode constants")?;
        }
        count = checked_count_add(
            count,
            statements_literal_count(&case.consequent)?,
            "bytecode constants",
        )?;
    }
    Ok(count)
}

fn declaration_literal_count(declaration: &HirVariableDeclaration) -> Result<usize, CompileError> {
    let mut count = 0;
    for declarator in declaration.declarators.iter() {
        let initializer_count = declarator
            .initializer
            .as_ref()
            .map(expression_literal_count)
            .transpose()?
            .unwrap_or(0);
        count = checked_count_add(count, initializer_count, "bytecode constants")?;
    }
    Ok(count)
}

fn hir_binding_count(hir: &HirProgram) -> Result<usize, CompileError> {
    statements_binding_count(hir.statements())
}

/// Counts all lexical bindings as a checked upper bound for the lowering-time local stack.
fn statements_binding_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => declaration.declarators.len(),
            HirStatementKind::FunctionDeclaration(_) => 1,
            HirStatementKind::Block(statements) => statements_binding_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_binding_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_binding_count(core::slice::from_ref(alternate))?,
                        "local bindings",
                    )?;
                }
                nested
            }
            HirStatementKind::For {
                initializer, body, ..
            } => {
                let initializer = match initializer {
                    Some(HirForInitializer::Variable(declaration)) => declaration.declarators.len(),
                    _ => 0,
                };
                checked_count_add(
                    initializer,
                    statements_binding_count(core::slice::from_ref(body))?,
                    "local bindings",
                )?
            }
            HirStatementKind::Loop { body, .. } => {
                statements_binding_count(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => switch_binding_count(cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let nested = try_children_count(
                    block,
                    handler.as_ref(),
                    finalizer.as_deref(),
                    "local bindings",
                    statements_binding_count,
                )?;
                checked_count_add(
                    nested,
                    usize::from(
                        handler
                            .as_ref()
                            .is_some_and(|handler| handler.parameter.is_some()),
                    ),
                    "local bindings",
                )?
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "local bindings")?;
    }
    Ok(count)
}

/// Counts all case-block bindings as one conservative switch-scope capacity bound.
fn switch_binding_count(cases: &[HirSwitchCase]) -> Result<usize, CompileError> {
    let mut count = 0;
    for case in cases {
        count = checked_count_add(
            count,
            statements_binding_count(&case.consequent)?,
            "local bindings",
        )?;
    }
    Ok(count)
}

/// Counts every conditional label before bytecode construction so the builder's label vector stays fixed-size.
fn hir_label_count(hir: &HirProgram) -> Result<usize, CompileError> {
    statements_label_count(hir.statements())
}

/// Counts structured-statement and expression labels before builder allocation.
fn statements_label_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        match &statement.kind {
            HirStatementKind::Expression(expression) => {
                count = checked_count_add(
                    count,
                    expression_label_count(expression)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::Loop { test, body, .. } => {
                count = checked_count_add(count, 3, "bytecode labels")?;
                count = checked_count_add(count, expression_label_count(test)?, "bytecode labels")?;
                count = checked_count_add(
                    count,
                    statements_label_count(core::slice::from_ref(body))?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::VariableDeclaration(declaration) => {
                for initializer in declaration
                    .declarators
                    .iter()
                    .filter_map(|declarator| declarator.initializer.as_ref())
                {
                    count = checked_count_add(
                        count,
                        expression_label_count(initializer)?,
                        "bytecode labels",
                    )?;
                }
            }
            HirStatementKind::Return(argument) => {
                if let Some(argument) = argument {
                    count = checked_count_add(
                        count,
                        expression_label_count(argument)?,
                        "bytecode labels",
                    )?;
                }
            }
            HirStatementKind::Throw(argument) => {
                count =
                    checked_count_add(count, expression_label_count(argument)?, "bytecode labels")?;
            }
            HirStatementKind::Block(statements) => {
                count = checked_count_add(
                    count,
                    statements_label_count(statements)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                count = checked_count_add(count, expression_label_count(test)?, "bytecode labels")?;
                count = checked_count_add(count, 2, "bytecode labels")?;
                count = checked_count_add(
                    count,
                    statements_label_count(core::slice::from_ref(consequent))?,
                    "bytecode labels",
                )?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_label_count(core::slice::from_ref(alternate))?,
                        "bytecode labels",
                    )?;
                }
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                count = checked_count_add(
                    count,
                    for_label_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                count = checked_count_add(
                    count,
                    switch_label_count(discriminant, cases)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                count = checked_count_add(
                    count,
                    try_children_count(
                        block,
                        handler.as_ref(),
                        finalizer.as_deref(),
                        "bytecode labels",
                        statements_label_count,
                    )?,
                    "bytecode labels",
                )?;
                if handler.is_some() {
                    count = checked_count_add(count, 1, "bytecode labels")?;
                }
            }
            HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Empty => {}
        }
    }
    Ok(count)
}

/// Counts the condition, update, and exit labels plus labels nested in every loop component.
fn for_label_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = 3;
    match initializer {
        Some(HirForInitializer::Variable(declaration)) => {
            for expression in declaration
                .declarators
                .iter()
                .filter_map(|declarator| declarator.initializer.as_ref())
            {
                count = checked_count_add(
                    count,
                    expression_label_count(expression)?,
                    "bytecode labels",
                )?;
            }
        }
        Some(HirForInitializer::Expression(expression)) => {
            count = checked_count_add(
                count,
                expression_label_count(expression)?,
                "bytecode labels",
            )?;
        }
        None => {}
    }
    for expression in [test, update].into_iter().flatten() {
        count = checked_count_add(
            count,
            expression_label_count(expression)?,
            "bytecode labels",
        )?;
    }
    checked_count_add(
        count,
        statements_label_count(core::slice::from_ref(body))?,
        "bytecode labels",
    )
}

/// Reserves one label per clause, one shared end label, and every nested expression/body label.
fn switch_label_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = expression_label_count(discriminant)?;
    count = checked_count_add(count, cases.len(), "bytecode labels")?;
    count = checked_count_add(count, 1, "bytecode labels")?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count = checked_count_add(count, expression_label_count(test)?, "bytecode labels")?;
        }
        count = checked_count_add(
            count,
            statements_label_count(&case.consequent)?,
            "bytecode labels",
        )?;
    }
    Ok(count)
}

/// Counts expression statements whose values may update a structured script completion.
fn statements_expression_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(_) => 1,
            HirStatementKind::Block(statements) => statements_expression_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_expression_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_expression_count(core::slice::from_ref(alternate))?,
                        "entry completion instructions",
                    )?;
                }
                nested
            }
            HirStatementKind::For { body, .. } | HirStatementKind::Loop { body, .. } => {
                statements_expression_count(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => switch_expression_count(cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "entry completion instructions",
                statements_expression_count,
            )?,
            HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "entry completion instructions")?;
    }
    Ok(count)
}

/// Counts clause expression statements that can update script completion after dispatch.
fn switch_expression_count(cases: &[HirSwitchCase]) -> Result<usize, CompileError> {
    let mut count = 0;
    for case in cases {
        count = checked_count_add(
            count,
            statements_expression_count(&case.consequent)?,
            "entry completion instructions",
        )?;
    }
    Ok(count)
}

/// Counts switch and loop nodes as an exact-capacity upper bound for the active break-target stack.
fn statements_switch_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_switch_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut count = statements_switch_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_switch_count(core::slice::from_ref(alternate))?,
                        "switch control targets",
                    )?;
                }
                count
            }
            HirStatementKind::For { body, .. } | HirStatementKind::Loop { body, .. } => {
                checked_count_add(
                    1,
                    statements_switch_count(core::slice::from_ref(body))?,
                    "switch control targets",
                )?
            }
            HirStatementKind::Switch { cases, .. } => {
                let mut count = 1;
                for case in cases.iter() {
                    count = checked_count_add(
                        count,
                        statements_switch_count(&case.consequent)?,
                        "switch control targets",
                    )?;
                }
                count
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "switch control targets",
                statements_switch_count,
            )?,
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, nested, "switch control targets")?;
    }
    Ok(count)
}

/// Counts loop nesting as an exact-capacity upper bound for the active continue-target stack.
fn statements_loop_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_loop_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_loop_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_loop_count(core::slice::from_ref(alternate))?,
                        "loop continue targets",
                    )?;
                }
                nested
            }
            HirStatementKind::For { body, .. } | HirStatementKind::Loop { body, .. } => {
                checked_count_add(
                    1,
                    statements_loop_count(core::slice::from_ref(body))?,
                    "loop continue targets",
                )?
            }
            HirStatementKind::Switch { cases, .. } => {
                let mut nested = 0;
                for case in cases.iter() {
                    nested = checked_count_add(
                        nested,
                        statements_loop_count(&case.consequent)?,
                        "loop continue targets",
                    )?;
                }
                nested
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "loop continue targets",
                statements_loop_count,
            )?,
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, nested, "loop continue targets")?;
    }
    Ok(count)
}

/// Counts nested conditional arms, each of which consumes exactly two symbolic labels.
fn expression_label_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Object(properties) => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    expression_label_count(&property.value)?,
                    "bytecode labels",
                )?;
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count =
                        checked_count_add(count, expression_label_count(key)?, "bytecode labels")?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::Binary { left, right, .. } => checked_count_add(
            expression_label_count(left)?,
            expression_label_count(right)?,
            "bytecode labels",
        ),
        HirExpressionKind::Logical { left, right, .. } => {
            let nested = checked_count_add(
                expression_label_count(left)?,
                expression_label_count(right)?,
                "bytecode labels",
            )?;
            checked_count_add(nested, 1, "bytecode labels")
        }
        HirExpressionKind::StaticMember { object, .. } => expression_label_count(object),
        HirExpressionKind::ComputedMember { object, property } => checked_count_add(
            expression_label_count(object)?,
            expression_label_count(property)?,
            "bytecode labels",
        ),
        HirExpressionKind::Assignment {
            operator,
            target,
            value,
        } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => expression_label_count(object)?,
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_label_count(object)?,
                    expression_label_count(property)?,
                    "bytecode labels",
                )?,
            };
            let nested =
                checked_count_add(target, expression_label_count(value)?, "bytecode labels")?;
            if matches!(operator, HirAssignmentOperator::Logical(_)) {
                checked_count_add(nested, 1, "bytecode labels")
            } else {
                Ok(nested)
            }
        }
        HirExpressionKind::Update { target, .. } => match target {
            HirAssignmentTarget::Identifier(_) => Ok(0),
            HirAssignmentTarget::StaticMember { object, .. } => expression_label_count(object),
            HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                expression_label_count(object)?,
                expression_label_count(property)?,
                "bytecode labels",
            ),
        },
        HirExpressionKind::Unary { argument, .. } => expression_label_count(argument),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => {
            let nested = checked_count_add(
                expression_label_count(test)?,
                checked_count_add(
                    expression_label_count(consequent)?,
                    expression_label_count(alternate)?,
                    "bytecode labels",
                )?,
                "bytecode labels",
            )?;
            checked_count_add(nested, 2, "bytecode labels")
        }
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_label_count(callee)?;
            for argument in arguments.iter() {
                count =
                    checked_count_add(count, expression_label_count(argument)?, "bytecode labels")?;
            }
            Ok(count)
        }
        _ => Ok(0),
    }
}

fn expression_literal_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Object(properties) => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    expression_literal_count(&property.value)?,
                    "bytecode constants",
                )?;
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count = checked_count_add(
                        count,
                        expression_literal_count(key)?,
                        "bytecode constants",
                    )?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::Number(_) => Ok(1),
        HirExpressionKind::Binary { left, right, .. } => checked_count_add(
            expression_literal_count(left)?,
            expression_literal_count(right)?,
            "bytecode constants",
        ),
        HirExpressionKind::Logical { left, right, .. } => checked_count_add(
            expression_literal_count(left)?,
            expression_literal_count(right)?,
            "bytecode constants",
        ),
        HirExpressionKind::StaticMember { object, .. } => expression_literal_count(object),
        HirExpressionKind::ComputedMember { object, property } => checked_count_add(
            expression_literal_count(object)?,
            expression_literal_count(property)?,
            "bytecode constants",
        ),
        HirExpressionKind::Assignment { target, value, .. } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    expression_literal_count(object)?
                }
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_literal_count(object)?,
                    expression_literal_count(property)?,
                    "bytecode constants",
                )?,
            };
            checked_count_add(
                target,
                expression_literal_count(value)?,
                "bytecode constants",
            )
        }
        HirExpressionKind::Update { target, .. } => match target {
            HirAssignmentTarget::Identifier(_) => Ok(0),
            HirAssignmentTarget::StaticMember { object, .. } => expression_literal_count(object),
            HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                expression_literal_count(object)?,
                expression_literal_count(property)?,
                "bytecode constants",
            ),
        },
        HirExpressionKind::Unary { argument, .. } => expression_literal_count(argument),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => checked_count_add(
            expression_literal_count(test)?,
            checked_count_add(
                expression_literal_count(consequent)?,
                expression_literal_count(alternate)?,
                "bytecode constants",
            )?,
            "bytecode constants",
        ),
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_literal_count(callee)?;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_literal_count(argument)?,
                    "bytecode constants",
                )?;
            }
            Ok(count)
        }
        _ => Ok(0),
    }
}

fn checked_count_add(
    total: usize,
    next: usize,
    collection: &'static str,
) -> Result<usize, CompileError> {
    total
        .checked_add(next)
        .ok_or(CompileError::LoweringCapacityOverflow { collection })
}
