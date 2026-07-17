#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Immutable register bytecode data structures, encodings, and verification contracts.
//!
//! Instructions are word-coded and endian-independent: each `u32` is an in-memory logical word,
//! never a borrowed byte slice. This crate intentionally has no host I/O surface.

use core::fmt;
use std::sync::Arc;

const OPCODE_MASK: u8 = 0x3f;
const FORMAT_MASK: u8 = 0xc0;
const NORMAL_FORMAT: u8 = 0x40;
const WIDE_FORMAT: u8 = 0x80;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct RegisterId(u32);

impl RegisterId {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ConstantId(u32);

impl ConstantId {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct FunctionId(u32);

impl FunctionId {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct FeedbackSlot(u32);

impl FeedbackSlot {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct WordOffset(u32);

impl WordOffset {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// The semantic opcode, independent from its compact/normal/wide encoding form.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Opcode {
    Nop = 0,
    LoadImmediate = 1,
    LoadConstant = 2,
    Move = 3,
    Add = 4,
    Sub = 5,
    Mul = 6,
    Div = 7,
    StrictEqual = 8,
    Jump = 9,
    JumpIfFalse = 10,
    Return = 11,
    Throw = 12,
    Call = 13,
    CreateClosure = 14,
    LoadScope = 15,
    StoreScope = 16,
}

impl Opcode {
    #[must_use]
    pub const fn operand_count(self) -> usize {
        match self {
            Self::Nop => 0,
            Self::LoadImmediate
            | Self::LoadConstant
            | Self::Move
            | Self::JumpIfFalse
            | Self::Call
            | Self::CreateClosure
            | Self::LoadScope
            | Self::StoreScope => 2,
            Self::Add | Self::Sub | Self::Mul | Self::Div | Self::StrictEqual => 3,
            Self::Jump | Self::Return | Self::Throw => 1,
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Jump | Self::Return | Self::Throw)
    }

    const fn from_base(base: u8) -> Option<Self> {
        match base {
            0 => Some(Self::Nop),
            1 => Some(Self::LoadImmediate),
            2 => Some(Self::LoadConstant),
            3 => Some(Self::Move),
            4 => Some(Self::Add),
            5 => Some(Self::Sub),
            6 => Some(Self::Mul),
            7 => Some(Self::Div),
            8 => Some(Self::StrictEqual),
            9 => Some(Self::Jump),
            10 => Some(Self::JumpIfFalse),
            11 => Some(Self::Return),
            12 => Some(Self::Throw),
            13 => Some(Self::Call),
            14 => Some(Self::CreateClosure),
            15 => Some(Self::LoadScope),
            16 => Some(Self::StoreScope),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperandWidth {
    Compact,
    Normal,
    Wide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedInstruction {
    pub opcode: Opcode,
    pub width: OperandWidth,
    pub operands: [u32; 3],
    pub operand_count: u8,
    pub word_len: u8,
}

impl DecodedInstruction {
    #[must_use]
    pub const fn operand(self, index: usize) -> Option<u32> {
        if index < self.operand_count as usize {
            Some(self.operands[index])
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    InvalidOpcode {
        offset: WordOffset,
        raw: u8,
    },
    InvalidFormat {
        offset: WordOffset,
        raw: u8,
    },
    Truncated {
        offset: WordOffset,
        expected_words: usize,
        remaining_words: usize,
    },
    NonZeroReservedBits {
        offset: WordOffset,
        word: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    WrongOperandCount {
        opcode: Opcode,
        expected: usize,
        actual: usize,
    },
}

/// Encodes one semantic instruction using the smallest lossless operand form.
pub fn encode_instruction(opcode: Opcode, operands: &[u32]) -> Result<Vec<u32>, EncodeError> {
    if operands.len() != opcode.operand_count() {
        return Err(EncodeError::WrongOperandCount {
            opcode,
            expected: opcode.operand_count(),
            actual: operands.len(),
        });
    }

    let width = if operands.iter().all(|&operand| operand <= u8::MAX as u32) {
        OperandWidth::Compact
    } else if operands.iter().all(|&operand| operand <= u16::MAX as u32) {
        OperandWidth::Normal
    } else {
        OperandWidth::Wide
    };
    let base = opcode as u8;

    match width {
        OperandWidth::Compact => {
            let mut word = base as u32;
            for (index, operand) in operands.iter().copied().enumerate() {
                word |= operand << (8 * (index + 1));
            }
            Ok(vec![word])
        }
        OperandWidth::Normal => {
            let mut words = Vec::with_capacity(1 + operands.len().div_ceil(2));
            words.push((base | NORMAL_FORMAT) as u32);
            for pair in operands.chunks(2) {
                words.push(pair[0] | (pair.get(1).copied().unwrap_or(0) << 16));
            }
            Ok(words)
        }
        OperandWidth::Wide => {
            let mut words = Vec::with_capacity(1 + operands.len());
            words.push((base | WIDE_FORMAT) as u32);
            words.extend_from_slice(operands);
            Ok(words)
        }
    }
}

/// Decodes exactly one instruction and validates all format-level invariants before returning it.
pub fn decode_instruction(
    words: &[u32],
    offset: WordOffset,
) -> Result<DecodedInstruction, DecodeError> {
    let start = offset.index() as usize;
    let header = *words.get(start).ok_or(DecodeError::Truncated {
        offset,
        expected_words: 1,
        remaining_words: 0,
    })?;
    let raw = header as u8;
    let opcode =
        Opcode::from_base(raw & OPCODE_MASK).ok_or(DecodeError::InvalidOpcode { offset, raw })?;
    let operand_count = opcode.operand_count();
    let format = raw & FORMAT_MASK;

    if format == 0xc0 {
        return Err(DecodeError::InvalidFormat { offset, raw });
    }
    if format != 0 && header & 0xffff_ff00 != 0 {
        return Err(DecodeError::NonZeroReservedBits {
            offset,
            word: header,
        });
    }

    let mut operands = [0; 3];
    match format {
        0 => {
            for (index, operand) in operands.iter_mut().take(operand_count).enumerate() {
                *operand = (header >> (8 * (index + 1))) & 0xff;
            }
            Ok(DecodedInstruction {
                opcode,
                width: OperandWidth::Compact,
                operands,
                operand_count: operand_count as u8,
                word_len: 1,
            })
        }
        NORMAL_FORMAT => decode_normal(words, offset, opcode, operands),
        WIDE_FORMAT => decode_wide(words, offset, opcode, operands),
        _ => Err(DecodeError::InvalidFormat { offset, raw }),
    }
}

fn decode_normal(
    words: &[u32],
    offset: WordOffset,
    opcode: Opcode,
    mut operands: [u32; 3],
) -> Result<DecodedInstruction, DecodeError> {
    let operand_count = opcode.operand_count();
    let data_words = operand_count.div_ceil(2);
    let expected_words = 1 + data_words;
    let start = offset.index() as usize;
    if words.len().saturating_sub(start) < expected_words {
        return Err(DecodeError::Truncated {
            offset,
            expected_words,
            remaining_words: words.len().saturating_sub(start),
        });
    }
    for index in 0..operand_count {
        operands[index] = (words[start + 1 + index / 2] >> (16 * (index % 2))) & 0xffff;
    }
    if operand_count % 2 == 1 && words[start + data_words] >> 16 != 0 {
        return Err(DecodeError::NonZeroReservedBits {
            offset: WordOffset::new(offset.index() + data_words as u32),
            word: words[start + data_words],
        });
    }
    Ok(DecodedInstruction {
        opcode,
        width: OperandWidth::Normal,
        operands,
        operand_count: operand_count as u8,
        word_len: expected_words as u8,
    })
}

fn decode_wide(
    words: &[u32],
    offset: WordOffset,
    opcode: Opcode,
    mut operands: [u32; 3],
) -> Result<DecodedInstruction, DecodeError> {
    let operand_count = opcode.operand_count();
    let expected_words = 1 + operand_count;
    let start = offset.index() as usize;
    if words.len().saturating_sub(start) < expected_words {
        return Err(DecodeError::Truncated {
            offset,
            expected_words,
            remaining_words: words.len().saturating_sub(start),
        });
    }
    operands[..operand_count].copy_from_slice(&words[start + 1..start + expected_words]);
    Ok(DecodedInstruction {
        opcode,
        width: OperandWidth::Wide,
        operands,
        operand_count: operand_count as u8,
        word_len: expected_words as u8,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifyContext {
    pub register_count: u32,
    pub constant_count: u32,
    pub function_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifyError {
    Decode(DecodeError),
    RegisterOutOfRange {
        offset: WordOffset,
        register: u32,
        register_count: u32,
    },
    ConstantOutOfRange {
        offset: WordOffset,
        constant: u32,
        constant_count: u32,
    },
    FunctionOutOfRange {
        offset: WordOffset,
        function: u32,
        function_count: u32,
    },
    InvalidJumpTarget {
        offset: WordOffset,
        target: u32,
    },
    MissingTerminalInstruction,
}

#[derive(Clone, Debug)]
pub struct Bytecode {
    words: Arc<[u32]>,
}

impl Bytecode {
    #[must_use]
    pub fn from_words(words: Vec<u32>) -> Self {
        Self {
            words: words.into(),
        }
    }
    #[must_use]
    pub fn words(&self) -> &[u32] {
        &self.words
    }
    pub fn verify(&self, context: VerifyContext) -> Result<VerifiedBytecode, VerifyError> {
        verify(self.clone(), context)
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedBytecode {
    bytecode: Bytecode,
    starts: Arc<[bool]>,
}

impl VerifiedBytecode {
    #[must_use]
    pub fn bytecode(&self) -> &Bytecode {
        &self.bytecode
    }
    #[must_use]
    pub fn is_instruction_start(&self, offset: WordOffset) -> bool {
        self.starts
            .get(offset.index() as usize)
            .copied()
            .unwrap_or(false)
    }
}

/// Verifies the complete stream in two passes so jumps can only land on decoded instruction boundaries.
fn verify(bytecode: Bytecode, context: VerifyContext) -> Result<VerifiedBytecode, VerifyError> {
    let words = bytecode.words();
    let mut starts = vec![false; words.len()];
    let mut offsets = Vec::new();
    let mut offset = 0usize;
    let mut last_opcode = None;
    while offset < words.len() {
        let word_offset = WordOffset::new(offset as u32);
        let decoded = decode_instruction(words, word_offset).map_err(VerifyError::Decode)?;
        starts[offset] = true;
        offsets.push((word_offset, decoded));
        last_opcode = Some(decoded.opcode);
        offset += decoded.word_len as usize;
    }
    if !matches!(last_opcode, Some(opcode) if opcode.is_terminal()) {
        return Err(VerifyError::MissingTerminalInstruction);
    }
    for (offset, decoded) in offsets {
        verify_instruction(decoded, offset, &starts, context)?;
    }
    Ok(VerifiedBytecode {
        bytecode,
        starts: starts.into(),
    })
}

fn verify_instruction(
    instruction: DecodedInstruction,
    offset: WordOffset,
    starts: &[bool],
    context: VerifyContext,
) -> Result<(), VerifyError> {
    let operands = instruction.operands;
    let check_register = |register| {
        if register < context.register_count {
            Ok(())
        } else {
            Err(VerifyError::RegisterOutOfRange {
                offset,
                register,
                register_count: context.register_count,
            })
        }
    };
    match instruction.opcode {
        Opcode::Nop | Opcode::Jump => {}
        Opcode::LoadImmediate | Opcode::LoadConstant | Opcode::LoadScope => {
            check_register(operands[0])?
        }
        Opcode::Move => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::Add
        | Opcode::Sub
        | Opcode::Mul
        | Opcode::Div
        | Opcode::StrictEqual
        | Opcode::Call => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            check_register(operands[2])?;
        }
        Opcode::JumpIfFalse => check_register(operands[0])?,
        Opcode::Return | Opcode::Throw => check_register(operands[0])?,
        Opcode::CreateClosure => check_register(operands[0])?,
        Opcode::StoreScope => check_register(operands[0])?,
    }
    if instruction.opcode == Opcode::LoadConstant && operands[1] >= context.constant_count {
        return Err(VerifyError::ConstantOutOfRange {
            offset,
            constant: operands[1],
            constant_count: context.constant_count,
        });
    }
    if instruction.opcode == Opcode::CreateClosure && operands[1] >= context.function_count {
        return Err(VerifyError::FunctionOutOfRange {
            offset,
            function: operands[1],
            function_count: context.function_count,
        });
    }
    if matches!(instruction.opcode, Opcode::Jump | Opcode::JumpIfFalse)
        && !starts
            .get(if instruction.opcode == Opcode::Jump {
                operands[0]
            } else {
                operands[1]
            } as usize)
            .copied()
            .unwrap_or(false)
    {
        return Err(VerifyError::InvalidJumpTarget {
            offset,
            target: if instruction.opcode == Opcode::Jump {
                operands[0]
            } else {
                operands[1]
            },
        });
    }
    Ok(())
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> VerifyContext {
        VerifyContext {
            register_count: 4,
            constant_count: 2,
            function_count: 1,
        }
    }
    #[test]
    fn encoding_selects_all_widths() {
        assert_eq!(
            decode_instruction(
                &encode_instruction(Opcode::Add, &[1, 2, 3]).unwrap(),
                WordOffset::new(0)
            )
            .unwrap()
            .width,
            OperandWidth::Compact
        );
        assert_eq!(
            decode_instruction(
                &encode_instruction(Opcode::Add, &[256, 2, 3]).unwrap(),
                WordOffset::new(0)
            )
            .unwrap()
            .width,
            OperandWidth::Normal
        );
        assert_eq!(
            decode_instruction(
                &encode_instruction(Opcode::Add, &[65_536, 2, 3]).unwrap(),
                WordOffset::new(0)
            )
            .unwrap()
            .width,
            OperandWidth::Wide
        );
    }
    #[test]
    fn verifier_rejects_operand_word_jump_target() {
        let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 256]).unwrap();
        words.extend(encode_instruction(Opcode::Jump, &[1]).unwrap());
        let error = Bytecode::from_words(words).verify(context()).unwrap_err();
        assert!(matches!(error, VerifyError::InvalidJumpTarget { .. }));
    }
    #[test]
    fn verifier_accepts_simple_terminal_program() {
        let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 1]).unwrap();
        words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
        assert!(Bytecode::from_words(words).verify(context()).is_ok());
    }
}
