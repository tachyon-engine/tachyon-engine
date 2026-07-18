//! Lowering of the first owned HIR subset into immutable register bytecode.

use tachyon_bytecode::{
    BytecodeBuilder, BytecodeConstant, CompiledFunctionTemplate, CompiledModule, FunctionId,
    FunctionKind, FunctionLayout, FunctionMetadata, MAX_ENCODED_INSTRUCTION_WORDS, Opcode,
    RegisterId, SourceSpan as BytecodeSourceSpan,
};

use crate::{
    CompileError, HirBinaryOperator, HirExpression, HirExpressionKind, HirFunction,
    HirFunctionDeclaration, HirProgram, HirStatement, HirStatementKind, HirUnaryOperator,
    HirVariableDeclaration, HirVariableDeclarationKind, ProgramKind, SourceName, SourceSpan,
    SourceText,
};

/// Lowers the currently supported HIR subset while preallocating builder and constant-pool storage from HIR counts.
pub(crate) fn lower(source: &SourceText, hir: &HirProgram) -> Result<CompiledModule, CompileError> {
    let mut constants = Vec::with_capacity(hir_literal_count(hir)?);
    let template_capacity =
        hir.functions()
            .len()
            .checked_add(1)
            .ok_or(CompileError::LoweringCapacityOverflow {
                collection: "compiled functions",
            })?;
    let mut templates = Vec::with_capacity(template_capacity);
    templates.push(lower_entry(source, hir, &mut constants)?);
    for function in hir.functions() {
        templates.push(lower_function(source, function, &mut constants)?);
    }
    CompiledModule::new(
        source.shared_text(),
        constants,
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
) -> Result<CompiledFunctionTemplate, CompileError> {
    let has_control_flow = hir.statements().iter().any(|statement| {
        matches!(
            statement.kind,
            HirStatementKind::Block(_) | HirStatementKind::If { .. } | HirStatementKind::Throw(_)
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
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(word_capacity, hir_label_count(hir)?),
        constants,
        locals: Vec::with_capacity(hir_binding_count(hir)?),
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
            ..FunctionLayout::default()
        },
        source_map,
        handlers: Default::default(),
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
) -> Result<CompiledFunctionTemplate, CompileError> {
    let instruction_capacity = statements_instruction_count(&function.body)?
        .checked_add(2)
        .and_then(|count| count.checked_mul(MAX_ENCODED_INSTRUCTION_WORDS))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function bytecode words",
        })?;
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(
            instruction_capacity,
            statements_label_count(&function.body)?,
        ),
        constants,
        locals: Vec::with_capacity(
            function
                .parameters
                .len()
                .checked_add(statements_binding_count(&function.body)?)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "function local bindings",
                })?,
        ),
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
                ..FunctionLayout::default()
            },
            source_map,
            handlers: Default::default(),
            suspend_points: Default::default(),
            feedback_sites: Default::default(),
        },
    ))
}

struct Lowerer<'a> {
    builder: BytecodeBuilder,
    constants: &'a mut Vec<BytecodeConstant>,
    locals: Vec<LocalBinding>,
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
            HirExpressionKind::Identifier(name) => self
                .local(name)
                .map(|binding| binding.register)
                .ok_or_else(|| CompileError::UnsupportedSyntax {
                    source_name: self.source_name.clone(),
                    span: expression.span,
                    syntax: "unresolved identifier",
                }),
            HirExpressionKind::Assignment { target, value } => {
                let binding =
                    self.local(target)
                        .cloned()
                        .ok_or_else(|| CompileError::UnsupportedSyntax {
                            source_name: self.source_name.clone(),
                            span: expression.span,
                            syntax: "unresolved assignment target",
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

    /// Evaluates callee/arguments in source order and copies them into the verified contiguous call window.
    fn call_expression(
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
            Opcode::Call,
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
}

fn hir_instruction_count(hir: &HirProgram) -> Result<usize, CompileError> {
    statements_instruction_count(hir.statements())
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
            HirStatementKind::FunctionDeclaration(_) => 1,
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
            HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode instructions")?;
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
        HirExpressionKind::Assignment { value, .. } => checked_count_add(
            expression_instruction_count(value)?,
            1,
            "bytecode instructions",
        ),
        HirExpressionKind::Unary { argument, .. } => expression_instruction_count(argument),
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
            HirStatementKind::FunctionDeclaration(_) => 0,
            HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode constants")?;
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
            HirStatementKind::Expression(_)
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "local bindings")?;
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
            HirStatementKind::FunctionDeclaration(_) | HirStatementKind::Empty => {}
        }
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
            HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "entry completion instructions")?;
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
        HirExpressionKind::Assignment { value, .. } => expression_label_count(value),
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
        HirExpressionKind::Assignment { value, .. } => expression_literal_count(value),
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
