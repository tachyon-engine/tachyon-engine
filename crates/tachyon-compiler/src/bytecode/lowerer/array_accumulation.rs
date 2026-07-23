//! Array literal accumulation lowering, including iterator-driven spread elements.

use super::*;
use crate::hir::HirArrayExpressionPart;

impl Lowerer<'_> {
    /// Lowers ArrayAccumulation directly so spread consumes iterators without observable concat.
    pub(super) fn array_accumulation(
        &mut self,
        parts: &[HirArrayExpressionPart],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let array = self.register()?;
        self.emit(Opcode::CreateArray, &[array.index()], span)?;
        let index = self.load_immediate(0, span)?;
        let one = self.load_immediate(1, span)?;
        for part in parts {
            match part {
                HirArrayExpressionPart::Element(expression) => {
                    let value = self.expression(expression)?;
                    self.emit(
                        Opcode::CreateDataPropertyByValue,
                        &[array.index(), value.index(), index.index()],
                        expression.span,
                    )?;
                    self.increment_array_accumulation_index(index, one, expression.span)?;
                }
                HirArrayExpressionPart::Elision => {
                    self.increment_array_accumulation_index(index, one, span)?;
                    let length = self.scope_name(&"length".into())?;
                    self.emit(
                        Opcode::SetById,
                        &[array.index(), index.index(), length],
                        span,
                    )?;
                }
                HirArrayExpressionPart::Spread(expression) => {
                    self.spread_array_element(array, index, one, expression)?;
                }
            }
        }
        Ok(array)
    }

    /// Emits one iterator loop and commits its cursor only after each own-property definition.
    fn spread_array_element(
        &mut self,
        array: RegisterId,
        index: RegisterId,
        one: RegisterId,
        expression: &HirExpression,
    ) -> Result<(), CompileError> {
        let source = self.expression(expression)?;
        let iterator = self.get_sync_iterator(source, expression.span)?;
        let loop_start = self.builder.new_label().map_err(CompileError::Builder)?;
        let loop_end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .bind_label(loop_start)
            .map_err(CompileError::Builder)?;
        let next = self.iterator_next(iterator, expression.span)?;
        self.builder
            .emit_jump_if_true(
                iterator.done,
                loop_end,
                BytecodeSourceSpan {
                    start: expression.span.start,
                    end: expression.span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let value = self.pattern_property(
            next,
            &HirObjectPropertyKey::Static("value".into()),
            expression.span,
        )?;
        self.emit(
            Opcode::CreateDataPropertyByValue,
            &[array.index(), value.index(), index.index()],
            expression.span,
        )?;
        self.increment_array_accumulation_index(index, one, expression.span)?;
        self.emit_jump(loop_start, expression.span)?;
        self.builder
            .bind_label(loop_end)
            .map_err(CompileError::Builder)
    }

    /// Advances the explicit ArrayAccumulation cursor without relying on host integer state.
    fn increment_array_accumulation_index(
        &mut self,
        index: RegisterId,
        one: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let next = self.emit_binary(HirBinaryOperator::Add, index, one, span)?;
        self.emit(Opcode::Move, &[index.index(), next.index()], span)
    }
}
