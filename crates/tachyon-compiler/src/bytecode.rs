//! Lowering of the first owned HIR subset into immutable register bytecode.

use tachyon_bytecode::{
    BytecodeBuilder, BytecodeConstant, CompiledFunctionTemplate, CompiledModule, FunctionId,
    FunctionKind, FunctionLayout, FunctionMetadata, HandlerEntry, HandlerKind, Label,
    MAX_ENCODED_INSTRUCTION_WORDS, Opcode, RegisterId, SourceSpan as BytecodeSourceSpan,
};

use crate::hir::HirAssignmentTarget;
use crate::{
    CompileError, HirBinaryOperator, HirCatchClause, HirExpression, HirExpressionKind, HirFunction,
    HirFunctionDeclaration, HirLogicalOperator, HirProgram, HirStatement, HirStatementKind,
    HirSwitchCase, HirUnaryOperator, HirVariableDeclaration, HirVariableDeclarationKind,
    ProgramKind, SourceName, SourceSpan, SourceText,
};

/// Lowers the currently supported HIR subset while preallocating builder and constant-pool storage from HIR counts.
pub(crate) fn lower(source: &SourceText, hir: &HirProgram) -> Result<CompiledModule, CompileError> {
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
    templates.push(lower_entry(source, hir, &mut constants, &mut scope_names)?);
    for function in hir.functions() {
        templates.push(lower_function(
            source,
            function,
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
    constants: &mut Vec<BytecodeConstant>,
    scope_names: &mut Vec<std::sync::Arc<str>>,
) -> Result<CompiledFunctionTemplate, CompileError> {
    let has_control_flow = hir.statements().iter().any(|statement| {
        matches!(
            statement.kind,
            HirStatementKind::Block(_)
                | HirStatementKind::If { .. }
                | HirStatementKind::Switch { .. }
                | HirStatementKind::Try { .. }
                | HirStatementKind::Break
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
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(word_capacity, hir_label_count(hir)?),
        constants,
        scope_names,
        locals: Vec::with_capacity(hir_binding_count(hir)?),
        break_targets: Vec::with_capacity(statements_switch_count(hir.statements())?),
        handlers: Vec::with_capacity(handler_count),
        next_register: 0,
        source_name: source.name().clone(),
    };
    for statement in hir.statements() {
        if let HirStatementKind::FunctionDeclaration(declaration) = &statement.kind {
            lowerer.function_declaration(declaration, statement.span)?;
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
                        | HirStatementKind::If { .. }
                        | HirStatementKind::Switch { .. }
                        | HirStatementKind::Try { .. }
                        | HirStatementKind::Break
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
    let (bytecode, source_map, register_count) =
        lowerer.builder.finish().map_err(CompileError::Builder)?;
    let kind = match hir.kind() {
        ProgramKind::Script => FunctionKind::Script,
        ProgramKind::Module => FunctionKind::Module,
        ProgramKind::CommonJs => FunctionKind::Script,
    };
    let metadata = FunctionMetadata {
        kind,
        layout: FunctionLayout {
            register_count,
            max_handler_depth,
            ..FunctionLayout::default()
        },
        source_map,
        handlers,
        suspend_points: Default::default(),
        feedback_sites: Default::default(),
    };
    Ok(CompiledFunctionTemplate::new(
        FunctionId::new(0),
        bytecode,
        metadata,
    ))
}

/// Lowers one ordinary function with parameter registers fixed at the front of its frame.
fn lower_function(
    source: &SourceText,
    function: &HirFunction,
    constants: &mut Vec<BytecodeConstant>,
    scope_names: &mut Vec<std::sync::Arc<str>>,
) -> Result<CompiledFunctionTemplate, CompileError> {
    let instruction_capacity = statements_instruction_count(&function.body)?
        .checked_add(2)
        .and_then(|count| count.checked_mul(MAX_ENCODED_INSTRUCTION_WORDS))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function bytecode words",
        })?;
    let handler_count = statements_handler_count(&function.body)?;
    let max_handler_depth = statements_handler_depth(&function.body)?;
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(
            instruction_capacity,
            statements_label_count(&function.body)?,
        ),
        constants,
        scope_names,
        locals: Vec::with_capacity(
            function
                .parameters
                .len()
                .checked_add(statements_binding_count(&function.body)?)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "function local bindings",
                })?,
        ),
        break_targets: Vec::with_capacity(statements_switch_count(&function.body)?),
        handlers: Vec::with_capacity(handler_count),
        next_register: 0,
        source_name: source.name().clone(),
    };
    for parameter in function.parameters.iter() {
        let register = lowerer.register()?;
        lowerer.locals.push(LocalBinding {
            name: parameter.name.clone(),
            register,
            mutable: true,
        });
    }
    let mut terminal = false;
    for statement in function.body.iter() {
        terminal = lowerer.function_statement(statement)?;
        if terminal {
            break;
        }
    }
    if !terminal {
        let undefined = lowerer.load_undefined(function.span)?;
        lowerer.emit(Opcode::Return, &[undefined.index()], function.span)?;
    }
    let handlers = freeze_handlers(lowerer.handlers)?;
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
            layout: FunctionLayout {
                register_count,
                argument_count: u32::try_from(function.parameters.len())
                    .map_err(|_| CompileError::RegisterOverflow)?,
                max_handler_depth,
                ..FunctionLayout::default()
            },
            source_map,
            handlers,
            suspend_points: Default::default(),
            feedback_sites: Default::default(),
        },
    ))
}

struct Lowerer<'a> {
    builder: BytecodeBuilder,
    constants: &'a mut Vec<BytecodeConstant>,
    scope_names: &'a mut Vec<std::sync::Arc<str>>,
    locals: Vec<LocalBinding>,
    break_targets: Vec<Label>,
    handlers: Vec<Option<HandlerEntry>>,
    next_register: u32,
    source_name: SourceName,
}

#[derive(Clone, Debug)]
struct LocalBinding {
    name: std::sync::Arc<str>,
    register: RegisterId,
    mutable: bool,
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
        let scope_name = self.scope_name(&declaration.binding.name)?;
        self.emit(Opcode::StoreScope, &[register.index(), scope_name], span)?;
        self.locals.push(LocalBinding {
            name: declaration.binding.name.clone(),
            register,
            mutable: true,
        });
        Ok(())
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
                let value = match argument {
                    Some(argument) => self.expression(argument)?,
                    None => self.load_undefined(statement.span)?,
                };
                self.emit(Opcode::Return, &[value.index()], statement.span)?;
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
            HirStatementKind::Empty => Ok(false),
            HirStatementKind::FunctionDeclaration(_) => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: statement.span,
                syntax: "nested function declaration",
            }),
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
        self.emit_jump(end, span)?;
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
        self.emit_jump(end, span)?;
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
            self.locals.push(LocalBinding {
                name: parameter.name.clone(),
                register: exception,
                mutable: true,
            });
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
            HirExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let opcode = match operator {
                    HirBinaryOperator::Add => Opcode::Add,
                    HirBinaryOperator::Subtract => Opcode::Sub,
                    HirBinaryOperator::Multiply => Opcode::Mul,
                    HirBinaryOperator::Divide => Opcode::Div,
                    HirBinaryOperator::StrictEqual => Opcode::StrictEqual,
                    _ => {
                        return Err(CompileError::UnsupportedSyntax {
                            source_name: self.source_name.clone(),
                            span: expression.span,
                            syntax: "binary operator",
                        });
                    }
                };
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                let destination = self.register()?;
                self.emit(
                    opcode,
                    &[destination.index(), left.index(), right.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Logical {
                operator,
                left,
                right,
            } => self.logical(*operator, left, right, expression.span),
            HirExpressionKind::Identifier(name) => match self.local(name) {
                Some(binding) => Ok(binding.register),
                None => {
                    let destination = self.register()?;
                    let scope_name = self.scope_name(name)?;
                    self.emit(
                        Opcode::LoadScope,
                        &[destination.index(), scope_name],
                        expression.span,
                    )?;
                    Ok(destination)
                }
            },
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
            HirExpressionKind::Assignment { target, value } => match target {
                HirAssignmentTarget::Identifier(target) => {
                    let binding = self.local(target).cloned().ok_or_else(|| {
                        CompileError::UnsupportedSyntax {
                            source_name: self.source_name.clone(),
                            span: expression.span,
                            syntax: "unresolved assignment target",
                        }
                    })?;
                    if !binding.mutable {
                        return Err(CompileError::UnsupportedSyntax {
                            source_name: self.source_name.clone(),
                            span: expression.span,
                            syntax: "assignment to immutable local",
                        });
                    }
                    let value = self.expression(value)?;
                    self.emit(
                        Opcode::Move,
                        &[binding.register.index(), value.index()],
                        expression.span,
                    )?;
                    Ok(value)
                }
                HirAssignmentTarget::StaticMember { object, property } => {
                    let receiver = self.expression(object)?;
                    let value = self.expression(value)?;
                    let property = self.scope_name(property)?;
                    self.emit(
                        Opcode::SetById,
                        &[receiver.index(), value.index(), property],
                        expression.span,
                    )?;
                    Ok(value)
                }
            },
            HirExpressionKind::Conditional {
                test,
                consequent,
                alternate,
            } => self.conditional(test, consequent, alternate, expression.span),
            HirExpressionKind::Call { callee, arguments } => {
                self.call_expression(callee, arguments, expression.span)
            }
            _ => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: expression.span,
                syntax: "expression",
            }),
        }
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
            self.locals.push(LocalBinding {
                name: declarator.binding.name.clone(),
                register,
                mutable: declaration.kind == HirVariableDeclarationKind::Let,
            });
        }
        Ok(())
    }

    #[inline(always)]
    fn local(&self, name: &str) -> Option<&LocalBinding> {
        self.locals
            .iter()
            .rev()
            .find(|binding| binding.name.as_ref() == name)
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
    let mut count = statements_scope_name_count(hir.statements())?;
    for function in hir.functions() {
        count = checked_count_add(
            count,
            statements_scope_name_count(&function.body)?,
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
            HirStatementKind::Break | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "scope names")?;
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
        HirExpressionKind::StaticMember { object, .. } => {
            checked_count_add(expression_scope_name_count(object)?, 1, "scope names")
        }
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
        HirExpressionKind::Assignment { target, value } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    checked_count_add(expression_scope_name_count(object)?, 1, "scope names")?
                }
            };
            checked_count_add(target, expression_scope_name_count(value)?, "scope names")
        }
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
        HirExpressionKind::Call { callee, arguments } => {
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
        | HirExpressionKind::Null => Ok(0),
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
            HirStatementKind::Break => 1,
            HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode instructions")?;
    }
    Ok(count)
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
        let initializer_count = declarator
            .initializer
            .as_ref()
            .map(expression_instruction_count)
            .transpose()?
            .unwrap_or(1);
        count = checked_count_add(count, initializer_count, "bytecode instructions")?;
    }
    Ok(count)
}

fn expression_instruction_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Binary { left, right, .. } => {
            let operands = checked_count_add(
                expression_instruction_count(left)?,
                expression_instruction_count(right)?,
                "bytecode instructions",
            )?;
            checked_count_add(1, operands, "bytecode instructions")
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
        HirExpressionKind::Assignment { target, value } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    expression_instruction_count(object)?
                }
            };
            let operands = checked_count_add(
                target,
                expression_instruction_count(value)?,
                "bytecode instructions",
            )?;
            checked_count_add(operands, 1, "bytecode instructions")
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
        HirExpressionKind::Call { callee, arguments } => {
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
            HirStatementKind::Break | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode constants")?;
    }
    Ok(count)
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
            | HirStatementKind::Empty => {}
        }
    }
    Ok(count)
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

/// Counts switch nodes as an exact-capacity upper bound for the active break-target stack.
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
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, nested, "switch control targets")?;
    }
    Ok(count)
}

/// Counts nested conditional arms, each of which consumes exactly two symbolic labels.
fn expression_label_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
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
        HirExpressionKind::Assignment { target, value } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => expression_label_count(object)?,
            };
            checked_count_add(target, expression_label_count(value)?, "bytecode labels")
        }
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
        HirExpressionKind::Call { callee, arguments } => {
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
        HirExpressionKind::Assignment { target, value } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    expression_literal_count(object)?
                }
            };
            checked_count_add(
                target,
                expression_literal_count(value)?,
                "bytecode constants",
            )
        }
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
        HirExpressionKind::Call { callee, arguments } => {
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
