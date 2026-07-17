//! Lowering of the first owned HIR subset into immutable register bytecode.

use tachyon_bytecode::{
    BytecodeBuilder, BytecodeConstant, CompiledFunctionTemplate, CompiledModule, FunctionId,
    FunctionKind, FunctionLayout, FunctionMetadata, MAX_ENCODED_INSTRUCTION_WORDS, Opcode,
    RegisterId, SourceSpan as BytecodeSourceSpan,
};

use crate::{
    CompileError, HirBinaryOperator, HirExpression, HirExpressionKind, HirProgram,
    HirStatementKind, HirUnaryOperator, HirVariableDeclaration, HirVariableDeclarationKind,
    ProgramKind, SourceName, SourceSpan, SourceText,
};

/// Lowers the currently supported HIR subset while preallocating builder and constant-pool storage from HIR counts.
pub(crate) fn lower(source: &SourceText, hir: &HirProgram) -> Result<CompiledModule, CompileError> {
    let has_expression = hir
        .statements()
        .iter()
        .any(|statement| matches!(&statement.kind, HirStatementKind::Expression(_)));
    let result_instruction_count = if has_expression { 1 } else { 2 };
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
        constants: Vec::with_capacity(hir_literal_count(hir)?),
        locals: Vec::with_capacity(hir_binding_count(hir)?),
        next_register: 0,
        source_name: source.name().clone(),
    };
    let result = match hir.statements() {
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
                    HirStatementKind::Empty => {}
                }
            }
            match result {
                Some(result) => result,
                None => lowerer.load_undefined(SourceSpan { start: 0, end: 0 })?,
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
    CompiledModule::new(
        source.shared_text(),
        lowerer.constants,
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            metadata,
        )],
        FunctionId::new(0),
    )
    .map_err(CompileError::Module)
}

struct Lowerer {
    builder: BytecodeBuilder,
    constants: Vec<BytecodeConstant>,
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

impl Lowerer {
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
            _ => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: expression.span,
                syntax: "expression",
            }),
        }
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
    let mut count = 0;
    for statement in hir.statements() {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_instruction_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                declaration_instruction_count(declaration)?
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
        _ => Ok(1),
    }
}

fn hir_literal_count(hir: &HirProgram) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in hir.statements() {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_literal_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                declaration_literal_count(declaration)?
            }
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
    let mut count = 0;
    for statement in hir.statements() {
        if let HirStatementKind::VariableDeclaration(declaration) = &statement.kind {
            count = checked_count_add(count, declaration.declarators.len(), "local bindings")?;
        }
    }
    Ok(count)
}

/// Counts every conditional label before bytecode construction so the builder's label vector stays fixed-size.
fn hir_label_count(hir: &HirProgram) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in hir.statements() {
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
            HirStatementKind::Empty => {}
        }
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
