use std::sync::Arc;

use super::{
    Bytecode, EncodeError, Opcode, RegisterId, WIDE_FORMAT, WordOffset, encode_instruction,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct Label(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceMapEntry {
    pub offset: WordOffset,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuilderError {
    Encode(EncodeError),
    InvalidLabel(Label),
    LabelAlreadyBound(Label),
    UnboundLabel(Label),
    LabelCountOverflow,
    CodeSizeOverflow,
    RegisterCountOverflow,
}

#[derive(Clone, Copy, Debug)]
struct JumpPatch {
    label: Label,
    operand_word: WordOffset,
}

/// A compiler-only mutable builder that resolves symbolic jumps before freezing words into `Bytecode`.
#[derive(Debug, Default)]
pub struct BytecodeBuilder {
    words: Vec<u32>,
    labels: Vec<Option<WordOffset>>,
    patches: Vec<JumpPatch>,
    source_map: Vec<SourceMapEntry>,
    register_count: u32,
}

impl BytecodeBuilder {
    #[must_use]
    pub fn with_capacity(word_capacity: usize, label_capacity: usize) -> Self {
        Self {
            words: Vec::with_capacity(word_capacity),
            labels: Vec::with_capacity(label_capacity),
            patches: Vec::new(),
            source_map: Vec::new(),
            register_count: 0,
        }
    }

    pub fn new_label(&mut self) -> Result<Label, BuilderError> {
        let label =
            Label(u32::try_from(self.labels.len()).map_err(|_| BuilderError::LabelCountOverflow)?);
        self.labels.push(None);
        Ok(label)
    }

    pub fn bind_label(&mut self, label: Label) -> Result<(), BuilderError> {
        let offset = self.next_word_offset()?;
        let Some(slot) = self.labels.get_mut(label.0 as usize) else {
            return Err(BuilderError::InvalidLabel(label));
        };
        if slot.is_some() {
            return Err(BuilderError::LabelAlreadyBound(label));
        }
        *slot = Some(offset);
        Ok(())
    }

    /// Returns the next instruction boundary for immutable side-table construction.
    pub fn current_offset(&self) -> Result<WordOffset, BuilderError> {
        self.next_word_offset()
    }

    /// Emits an instruction in its smallest representation and records its source span without retaining AST data.
    pub fn emit(
        &mut self,
        opcode: Opcode,
        operands: &[u32],
        span: SourceSpan,
    ) -> Result<WordOffset, BuilderError> {
        let words = encode_instruction(opcode, operands).map_err(BuilderError::Encode)?;
        self.ensure_word_capacity(words.len())?;
        self.note_registers(opcode, operands)?;
        let offset = self.next_word_offset()?;
        self.words.extend(words);
        self.source_map.push(SourceMapEntry { offset, span });
        Ok(offset)
    }

    /// Emits a wide `Jump` placeholder so label binding cannot alter instruction length or downstream offsets.
    pub fn emit_jump(
        &mut self,
        label: Label,
        span: SourceSpan,
    ) -> Result<WordOffset, BuilderError> {
        self.ensure_label(label)?;
        self.ensure_word_capacity(2)?;
        let offset = self.next_word_offset()?;
        self.words
            .extend([((Opcode::Jump as u8) | WIDE_FORMAT) as u32, 0]);
        self.patches.push(JumpPatch {
            label,
            operand_word: WordOffset::new(offset.index() + 1),
        });
        self.source_map.push(SourceMapEntry { offset, span });
        Ok(offset)
    }

    /// Emits a fixed-width abrupt target patched from one symbolic label.
    pub fn emit_abrupt_jump(
        &mut self,
        opcode: Opcode,
        label: Label,
        span: SourceSpan,
    ) -> Result<WordOffset, BuilderError> {
        debug_assert!(matches!(
            opcode,
            Opcode::BreakThroughFinally | Opcode::ContinueThroughFinally
        ));
        self.ensure_label(label)?;
        self.ensure_word_capacity(2)?;
        let offset = self.next_word_offset()?;
        self.words
            .extend([((opcode as u8) | WIDE_FORMAT) as u32, 0]);
        self.patches.push(JumpPatch {
            label,
            operand_word: WordOffset::new(offset.index() + 1),
        });
        self.source_map.push(SourceMapEntry { offset, span });
        Ok(offset)
    }

    /// Emits a wide conditional jump placeholder with an immutable condition register and a patchable target word.
    pub fn emit_jump_if_false(
        &mut self,
        condition: RegisterId,
        label: Label,
        span: SourceSpan,
    ) -> Result<WordOffset, BuilderError> {
        self.emit_conditional_jump(Opcode::JumpIfFalse, condition, label, span)
    }

    /// Emits a wide truthy conditional jump with a stable patchable target word.
    pub fn emit_jump_if_true(
        &mut self,
        condition: RegisterId,
        label: Label,
        span: SourceSpan,
    ) -> Result<WordOffset, BuilderError> {
        self.emit_conditional_jump(Opcode::JumpIfTrue, condition, label, span)
    }

    /// Emits a wide non-nullish conditional jump with a stable patchable target word.
    pub fn emit_jump_if_not_nullish(
        &mut self,
        condition: RegisterId,
        label: Label,
        span: SourceSpan,
    ) -> Result<WordOffset, BuilderError> {
        self.emit_conditional_jump(Opcode::JumpIfNotNullish, condition, label, span)
    }

    /// Records one register-and-target branch without allowing label patching to change its width.
    fn emit_conditional_jump(
        &mut self,
        opcode: Opcode,
        condition: RegisterId,
        label: Label,
        span: SourceSpan,
    ) -> Result<WordOffset, BuilderError> {
        debug_assert!(matches!(
            opcode,
            Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish
        ));
        self.ensure_label(label)?;
        self.note_register(condition.index())?;
        self.ensure_word_capacity(3)?;
        let offset = self.next_word_offset()?;
        self.words
            .extend([((opcode as u8) | WIDE_FORMAT) as u32, condition.index(), 0]);
        self.patches.push(JumpPatch {
            label,
            operand_word: WordOffset::new(offset.index() + 2),
        });
        self.source_map.push(SourceMapEntry { offset, span });
        Ok(offset)
    }

    /// Resolves every label once, then freezes the builder's words and source map without retaining spare capacity.
    pub fn finish(mut self) -> Result<(Bytecode, Arc<[SourceMapEntry]>, u32), BuilderError> {
        for patch in &self.patches {
            let target = self
                .labels
                .get(patch.label.0 as usize)
                .ok_or(BuilderError::InvalidLabel(patch.label))?
                .ok_or(BuilderError::UnboundLabel(patch.label))?;
            self.words[patch.operand_word.index() as usize] = target.index();
        }
        Ok((
            Bytecode::from_words(self.words),
            self.source_map.into(),
            self.register_count,
        ))
    }

    fn ensure_label(&self, label: Label) -> Result<(), BuilderError> {
        if self.labels.get(label.0 as usize).is_some() {
            Ok(())
        } else {
            Err(BuilderError::InvalidLabel(label))
        }
    }

    fn next_word_offset(&self) -> Result<WordOffset, BuilderError> {
        u32::try_from(self.words.len())
            .map(WordOffset::new)
            .map_err(|_| BuilderError::CodeSizeOverflow)
    }

    fn ensure_word_capacity(&self, additional: usize) -> Result<(), BuilderError> {
        self.words
            .len()
            .checked_add(additional)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or(BuilderError::CodeSizeOverflow)?;
        Ok(())
    }

    fn note_registers(&mut self, opcode: Opcode, operands: &[u32]) -> Result<(), BuilderError> {
        let indexes: &[usize] = match opcode {
            Opcode::Nop
            | Opcode::Jump
            | Opcode::EnterFinally
            | Opcode::ResumeCompletion
            | Opcode::BreakThroughFinally
            | Opcode::ContinueThroughFinally
            | Opcode::ReturnUndefined
            | Opcode::DeclareScope
            | Opcode::DeclareGlobalLexical => &[],
            Opcode::LoadUndefined
            | Opcode::LoadNull
            | Opcode::LoadFalse
            | Opcode::LoadTrue
            | Opcode::LoadImmediate
            | Opcode::LoadConstant
            | Opcode::LoadScope
            | Opcode::CreateClosure
            | Opcode::CreateBaseClass
            | Opcode::StoreScope
            | Opcode::StoreResolvedScope
            | Opcode::InitializeGlobalLexical
            | Opcode::Return
            | Opcode::Throw => &[0],
            Opcode::CreateObject
            | Opcode::CreateArray
            | Opcode::LoadException
            | Opcode::LoadThis
            | Opcode::LoadNewTarget
            | Opcode::LoadArgumentsLength
            | Opcode::LoadArgumentsObject
            | Opcode::InitializeThis
            | Opcode::SuperConstructForwardAll
            | Opcode::CheckConstructor => &[0],
            Opcode::Move
            | Opcode::Not
            | Opcode::Negate
            | Opcode::Typeof
            | Opcode::ToNumber
            | Opcode::BitwiseNot
            | Opcode::TypeofScope
            | Opcode::CreateForInIterator
            | Opcode::ForInNext
            | Opcode::CreateExclusionList
            | Opcode::ExcludePropertyKey
            | Opcode::CollectRestArguments => &[0, 1],
            Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish => &[0],
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::StrictEqual
            | Opcode::LessThan
            | Opcode::BitwiseAnd
            | Opcode::BitwiseOr
            | Opcode::BitwiseXor
            | Opcode::ShiftLeft
            | Opcode::ShiftRight
            | Opcode::ShiftRightUnsigned
            | Opcode::Remainder
            | Opcode::Exponentiate
            | Opcode::GreaterThan
            | Opcode::LessEqual
            | Opcode::GreaterEqual
            | Opcode::LooseEqual
            | Opcode::LooseNotEqual
            | Opcode::HasProperty
            | Opcode::DeleteById
            | Opcode::DeleteByValue
            | Opcode::InstanceOf
            | Opcode::GetByValue
            | Opcode::SetByValue
            | Opcode::ToPropertyKey
            | Opcode::ToPropertyKeyForIn
            | Opcode::DefineGetterById
            | Opcode::DefineSetterById
            | Opcode::DefineGetterByValue
            | Opcode::DefineSetterByValue
            | Opcode::CopyDataProperties => &[0, 1, 2],
            Opcode::SetAccessorFunctionName => &[0, 1],
            Opcode::SetFunctionName => &[0],
            Opcode::LoadEnvironment | Opcode::StoreEnvironment => &[0],
            Opcode::GetById | Opcode::SetById => &[0, 1],
            Opcode::DefineClassMethodById
            | Opcode::DefineClassGetterById
            | Opcode::DefineClassSetterById => &[0, 1],
            Opcode::Call | Opcode::Construct | Opcode::SuperConstruct => {
                for &index in &[0, 1] {
                    if let Some(&register) = operands.get(index) {
                        self.note_register(register)?;
                    }
                }
                let callee = operands[1];
                let argument_count = operands[2];
                if argument_count != 0 {
                    self.note_register(
                        callee
                            .checked_add(argument_count)
                            .ok_or(BuilderError::RegisterCountOverflow)?,
                    )?;
                }
                return Ok(());
            }
            Opcode::CreateClass => &[0, 2],
            Opcode::CallWithReceiver => {
                for &index in &[0, 1] {
                    if let Some(&register) = operands.get(index) {
                        self.note_register(register)?;
                    }
                }
                let receiver = operands[1];
                let argument_count = operands[2];
                self.note_register(
                    receiver
                        .checked_add(1)
                        .and_then(|callee| callee.checked_add(argument_count))
                        .ok_or(BuilderError::RegisterCountOverflow)?,
                )?;
                return Ok(());
            }
            Opcode::Await | Opcode::Yield => &[0, 1],
        };
        for &index in indexes {
            if let Some(&register) = operands.get(index) {
                self.note_register(register)?;
            }
        }
        Ok(())
    }

    fn note_register(&mut self, register: u32) -> Result<(), BuilderError> {
        let count = register
            .checked_add(1)
            .ok_or(BuilderError::RegisterCountOverflow)?;
        self.register_count = self.register_count.max(count);
        Ok(())
    }
}
