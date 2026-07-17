//! Lowering of the first owned HIR subset into immutable register bytecode.

use tachyon_bytecode::{
    BytecodeBuilder, BytecodeConstant, CompiledFunctionTemplate, CompiledModule, FunctionId,
    FunctionKind, FunctionLayout, FunctionMetadata, Opcode, RegisterId,
    SourceSpan as BytecodeSourceSpan,
};

use crate::{
    CompileError, HirBinaryOperator, HirExpression, HirExpressionKind, HirProgram,
    HirStatementKind, ProgramKind, SourceName, SourceSpan, SourceText,
};

/// Lowers the currently supported HIR subset while preallocating builder and constant-pool storage from HIR counts.
pub(crate) fn lower(source: &SourceText, hir: &HirProgram) -> Result<CompiledModule, CompileError> {
    let instruction_upper_bound = hir_instruction_count(hir).saturating_add(1);
    let mut lowerer = Lowerer {
        builder: BytecodeBuilder::with_capacity(instruction_upper_bound.saturating_mul(4), 0),
        constants: Vec::with_capacity(hir_literal_count(hir)),
        next_register: 0,
        source_name: source.name().clone(),
    };
    let result = match hir.statements() {
        [] => lowerer.load_immediate(0, SourceSpan { start: 0, end: 0 })?,
        statements => {
            let mut result = None;
            for statement in statements {
                if let HirStatementKind::Expression(expression) = &statement.kind {
                    result = Some(lowerer.expression(expression)?);
                }
            }
            match result {
                Some(result) => result,
                None => lowerer.load_immediate(0, SourceSpan { start: 0, end: 0 })?,
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
    next_register: u32,
    source_name: SourceName,
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
            _ => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: expression.span,
                syntax: "expression",
            }),
        }
    }

    fn load_immediate(&mut self, value: u32, span: SourceSpan) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        self.emit(Opcode::LoadImmediate, &[register.index(), value], span)?;
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

fn hir_instruction_count(hir: &HirProgram) -> usize {
    hir.statements()
        .iter()
        .map(|statement| match &statement.kind {
            HirStatementKind::Expression(expression) => expression_instruction_count(expression),
            HirStatementKind::Empty => 0,
        })
        .sum()
}

fn expression_instruction_count(expression: &HirExpression) -> usize {
    match &expression.kind {
        HirExpressionKind::Binary { left, right, .. } => {
            1 + expression_instruction_count(left) + expression_instruction_count(right)
        }
        _ => 1,
    }
}

fn hir_literal_count(hir: &HirProgram) -> usize {
    hir.statements()
        .iter()
        .map(|statement| match &statement.kind {
            HirStatementKind::Expression(expression) => expression_literal_count(expression),
            HirStatementKind::Empty => 0,
        })
        .sum()
}

fn expression_literal_count(expression: &HirExpression) -> usize {
    match &expression.kind {
        HirExpressionKind::Number(_) => 1,
        HirExpressionKind::Binary { left, right, .. } => {
            expression_literal_count(left) + expression_literal_count(right)
        }
        _ => 0,
    }
}
