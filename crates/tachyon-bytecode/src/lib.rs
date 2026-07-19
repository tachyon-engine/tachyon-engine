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

mod disassembler;

pub use disassembler::{DisassemblyError, disassemble};

const OPCODE_MASK: u8 = 0x3f;
const BASE_OPCODE_COUNT: usize = OPCODE_MASK as usize + 1;
const FORMAT_MASK: u8 = 0xc0;
const ESCAPE_FORMAT: u8 = 0xc0;
const NORMAL_FORMAT: u8 = 0x40;
const WIDE_FORMAT: u8 = 0x80;

/// The largest physical encoding: one header plus three wide operands.
pub const MAX_ENCODED_INSTRUCTION_WORDS: usize = 4;

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
    /// Copies register operand 1 into destination register operand 0.
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
    /// Calls register 1 with register 2 arguments stored contiguously after the callee.
    Call = 13,
    CreateClosure = 14,
    LoadScope = 15,
    StoreScope = 16,
    Await = 17,
    Yield = 18,
    LoadUndefined = 19,
    LoadNull = 20,
    LoadFalse = 21,
    LoadTrue = 22,
    Not = 23,
    JumpIfTrue = 24,
    JumpIfNotNullish = 25,
    Negate = 26,
    CreateObject = 27,
    GetById = 28,
    SetById = 29,
    /// Calls callee at receiver+1 with arguments following it while preserving `this`.
    CallWithReceiver = 30,
    /// Moves the fiber's pending thrown value into a catch-local register.
    LoadException = 31,
    /// Constructs register 1 with register 2 arguments stored contiguously after the callee.
    Construct = 32,
    LoadThis = 33,
    LoadNewTarget = 34,
    /// Performs the current numeric less-than subset over registers 1 and 2.
    LessThan = 35,
    /// Loads a property using a runtime key value.
    GetByValue = 36,
    /// Stores a property using a runtime key value.
    SetByValue = 37,
    /// Loads the ECMAScript typeof result for register 1.
    Typeof = 38,
    /// Creates a script-scoped var binding only when the global does not already exist.
    DeclareScope = 39,
    /// Tests register 1's ordinary prototype chain against callable register 2.
    InstanceOf = 40,
    /// Stores register 0 only when scope-name operand 1 already resolves.
    StoreResolvedScope = 41,
    /// Loads environment depth operand 1 and slot operand 2 into register operand 0.
    LoadEnvironment = 42,
    /// Stores register operand 0 into environment depth operand 1 and slot operand 2.
    StoreEnvironment = 43,
    /// Declares one global lexical binding from scope-name operand 0 and mutability operand 1.
    DeclareGlobalLexical = 44,
    /// Initializes global lexical scope-name operand 1 from register operand 0 exactly once.
    InitializeGlobalLexical = 45,
    /// Returns the ECMAScript undefined value without allocating a register.
    ReturnUndefined = 46,
    /// Converts a primitive value to the current numeric subset.
    ToNumber = 47,
    /// Applies ECMAScript ToInt32 and bitwise complement to a numeric value.
    BitwiseNot = 48,
    BitwiseAnd = 49,
    BitwiseOr = 50,
    BitwiseXor = 51,
    ShiftLeft = 52,
    ShiftRight = 53,
    ShiftRightUnsigned = 54,
    Remainder = 55,
    Exponentiate = 56,
    GreaterThan = 57,
    LessEqual = 58,
    GreaterEqual = 59,
    LooseEqual = 60,
    LooseNotEqual = 61,
    HasProperty = 62,
    TypeofScope = 63,
    DeleteById = 64,
    DeleteByValue = 65,
    CreateArray = 66,
    /// Snapshots enumerable string keys into one internal iterator object.
    CreateForInIterator = 67,
    /// Returns the next string key or undefined when the internal iterator is exhausted.
    ForInNext = 68,
    /// Loads the active function's exact actual-argument count into one register.
    LoadArgumentsLength = 69,
}

const OPCODE_COUNT: usize = 70;
const OPCODE_OPERAND_COUNTS: [u8; OPCODE_COUNT] = [
    0, // Nop
    2, // LoadImmediate
    2, // LoadConstant
    2, // Move
    3, // Add
    3, // Sub
    3, // Mul
    3, // Div
    3, // StrictEqual
    1, // Jump
    2, // JumpIfFalse
    1, // Return
    1, // Throw
    3, // Call
    2, // CreateClosure
    2, // LoadScope
    2, // StoreScope
    3, // Await
    3, // Yield
    1, // LoadUndefined
    1, // LoadNull
    1, // LoadFalse
    1, // LoadTrue
    2, // Not
    2, // JumpIfTrue
    2, // JumpIfNotNullish
    2, // Negate
    1, // CreateObject
    3, // GetById
    3, // SetById
    3, // CallWithReceiver
    1, // LoadException
    3, // Construct
    1, // LoadThis
    1, // LoadNewTarget
    3, // LessThan
    3, // GetByValue
    3, // SetByValue
    2, // Typeof
    1, // DeclareScope
    3, // InstanceOf
    2, // StoreResolvedScope
    3, // LoadEnvironment
    3, // StoreEnvironment
    2, // DeclareGlobalLexical
    2, // InitializeGlobalLexical
    0, // ReturnUndefined
    2, // ToNumber
    2, // BitwiseNot
    3, // BitwiseAnd
    3, // BitwiseOr
    3, // BitwiseXor
    3, // ShiftLeft
    3, // ShiftRight
    3, // ShiftRightUnsigned
    3, // Remainder
    3, // Exponentiate
    3, // GreaterThan
    3, // LessEqual
    3, // GreaterEqual
    3, // LooseEqual
    3, // LooseNotEqual
    3, // HasProperty
    2, // TypeofScope
    3, // DeleteById
    3, // DeleteByValue
    1, // CreateArray
    2, // CreateForInIterator
    2, // ForInNext
    1, // LoadArgumentsLength
];

const _: [(); OPCODE_COUNT] = [(); OPCODE_OPERAND_COUNTS.len()];
const _: [(); OPCODE_COUNT] = [(); Opcode::LoadArgumentsLength as usize + 1];

impl Opcode {
    /// Number of semantic opcodes represented by this bytecode version.
    pub const COUNT: usize = OPCODE_COUNT;

    /// Recovers an opcode from its dense profiling/disassembly index.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        if index >= Self::COUNT {
            return None;
        }
        if index < BASE_OPCODE_COUNT {
            Self::from_base(index as u8)
        } else {
            Self::from_extended_base((index - BASE_OPCODE_COUNT) as u8)
        }
    }

    #[must_use]
    pub const fn operand_count(self) -> usize {
        OPCODE_OPERAND_COUNTS[self as usize] as usize
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Jump | Self::Return | Self::ReturnUndefined | Self::Throw
        )
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
            17 => Some(Self::Await),
            18 => Some(Self::Yield),
            19 => Some(Self::LoadUndefined),
            20 => Some(Self::LoadNull),
            21 => Some(Self::LoadFalse),
            22 => Some(Self::LoadTrue),
            23 => Some(Self::Not),
            24 => Some(Self::JumpIfTrue),
            25 => Some(Self::JumpIfNotNullish),
            26 => Some(Self::Negate),
            27 => Some(Self::CreateObject),
            28 => Some(Self::GetById),
            29 => Some(Self::SetById),
            30 => Some(Self::CallWithReceiver),
            31 => Some(Self::LoadException),
            32 => Some(Self::Construct),
            33 => Some(Self::LoadThis),
            34 => Some(Self::LoadNewTarget),
            35 => Some(Self::LessThan),
            36 => Some(Self::GetByValue),
            37 => Some(Self::SetByValue),
            38 => Some(Self::Typeof),
            39 => Some(Self::DeclareScope),
            40 => Some(Self::InstanceOf),
            41 => Some(Self::StoreResolvedScope),
            42 => Some(Self::LoadEnvironment),
            43 => Some(Self::StoreEnvironment),
            44 => Some(Self::DeclareGlobalLexical),
            45 => Some(Self::InitializeGlobalLexical),
            46 => Some(Self::ReturnUndefined),
            47 => Some(Self::ToNumber),
            48 => Some(Self::BitwiseNot),
            49 => Some(Self::BitwiseAnd),
            50 => Some(Self::BitwiseOr),
            51 => Some(Self::BitwiseXor),
            52 => Some(Self::ShiftLeft),
            53 => Some(Self::ShiftRight),
            54 => Some(Self::ShiftRightUnsigned),
            55 => Some(Self::Remainder),
            56 => Some(Self::Exponentiate),
            57 => Some(Self::GreaterThan),
            58 => Some(Self::LessEqual),
            59 => Some(Self::GreaterEqual),
            60 => Some(Self::LooseEqual),
            61 => Some(Self::LooseNotEqual),
            62 => Some(Self::HasProperty),
            63 => Some(Self::TypeofScope),
            _ => None,
        }
    }

    const fn from_extended_base(base: u8) -> Option<Self> {
        match base {
            0 => Some(Self::DeleteById),
            1 => Some(Self::DeleteByValue),
            2 => Some(Self::CreateArray),
            3 => Some(Self::CreateForInIterator),
            4 => Some(Self::ForInNext),
            5 => Some(Self::LoadArgumentsLength),
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

    if (opcode as u8) >= 64 {
        let extension = (opcode as u8) - 64;
        let mut words = Vec::with_capacity(1 + operands.len());
        words.push(u32::from(ESCAPE_FORMAT | extension));
        words.extend_from_slice(operands);
        return Ok(words);
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
#[inline(always)]
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
    let format = raw & FORMAT_MASK;

    let opcode = if format == ESCAPE_FORMAT {
        Opcode::from_extended_base(raw & OPCODE_MASK)
    } else {
        Opcode::from_base(raw & OPCODE_MASK)
    }
    .ok_or(DecodeError::InvalidOpcode { offset, raw })?;
    let operand_count = opcode.operand_count();
    let operands = [0; 3];

    if format == ESCAPE_FORMAT {
        return decode_escape(words, offset, opcode, operands);
    }
    if format != 0 && header & 0xffff_ff00 != 0 {
        return Err(DecodeError::NonZeroReservedBits {
            offset,
            word: header,
        });
    }

    let mut operands = operands;
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

fn decode_escape(
    words: &[u32],
    offset: WordOffset,
    opcode: Opcode,
    mut operands: [u32; 3],
) -> Result<DecodedInstruction, DecodeError> {
    let operand_count = opcode.operand_count();
    let start = offset.index() as usize;
    let expected_words = 1 + operand_count;
    if words.len().saturating_sub(start) < expected_words {
        return Err(DecodeError::Truncated {
            offset,
            expected_words,
            remaining_words: words.len().saturating_sub(start),
        });
    }
    for (index, operand) in operands.iter_mut().take(operand_count).enumerate() {
        *operand = words[start + 1 + index];
    }
    Ok(DecodedInstruction {
        opcode,
        width: OperandWidth::Wide,
        operands,
        operand_count: operand_count as u8,
        word_len: expected_words as u8,
    })
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
    pub scope_name_count: u32,
    pub max_environment_slot_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifyError {
    Decode(DecodeError),
    CodeTooLarge {
        word_count: usize,
    },
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
    ScopeNameOutOfRange {
        offset: WordOffset,
        scope_name: u32,
        scope_name_count: u32,
    },
    FunctionOutOfRange {
        offset: WordOffset,
        function: u32,
        function_count: u32,
    },
    EnvironmentSlotOutOfRange {
        offset: WordOffset,
        slot: u32,
        max_environment_slot_count: u32,
    },
    InvalidBooleanOperand {
        offset: WordOffset,
        operand: u32,
    },
    InvalidCallArgumentWindow {
        offset: WordOffset,
        callee: u32,
        argument_count: u32,
        register_count: u32,
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
            | Opcode::LoadArgumentsLength => &[0],
            Opcode::Move
            | Opcode::Not
            | Opcode::Negate
            | Opcode::Typeof
            | Opcode::ToNumber
            | Opcode::BitwiseNot
            | Opcode::TypeofScope
            | Opcode::CreateForInIterator
            | Opcode::ForInNext => &[0, 1],
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
            | Opcode::SetByValue => &[0, 1, 2],
            Opcode::LoadEnvironment | Opcode::StoreEnvironment => &[0],
            Opcode::GetById | Opcode::SetById => &[0, 1],
            Opcode::Call | Opcode::Construct => {
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

/// A zero-allocation decoder for instruction boundaries proven by `VerifiedBytecode`.
///
/// Construction retains the verified bytecode lifetime, so its immutable word backing cannot be
/// replaced or released while this decoder is in use. Decoding is unsafe because an arbitrary word
/// offset does not carry the instruction-boundary proof stored in `VerifiedBytecode::starts`.
#[derive(Clone, Copy)]
pub struct VerifiedInstructionDecoder<'bytecode> {
    words: &'bytecode [u32],
}

impl<'bytecode> VerifiedInstructionDecoder<'bytecode> {
    /// Borrows the immutable words of one completely verified bytecode stream.
    #[must_use]
    pub fn new(bytecode: &'bytecode VerifiedBytecode) -> Self {
        Self {
            words: bytecode.bytecode().words(),
        }
    }

    /// Decodes one verifier-proven instruction without repeating format or bounds validation.
    ///
    /// # Safety
    ///
    /// `offset` must be an instruction start in the same `VerifiedBytecode` used to construct this
    /// decoder. In particular, `VerifiedBytecode::is_instruction_start(offset)` must return `true`.
    /// Verification guarantees that the header contains a valid opcode and encoding format, every
    /// encoded operand word is present, and reserved bits satisfy the encoding contract.
    #[must_use]
    #[inline(always)]
    pub unsafe fn decode_unchecked(&self, offset: WordOffset) -> DecodedInstruction {
        let start = offset.index() as usize;
        // SAFETY: the caller guarantees `start` is a verified instruction boundary.
        let header = unsafe { *self.words.get_unchecked(start) };
        let raw = header as u8;
        let format = raw & FORMAT_MASK;
        let semantic_opcode = (raw & OPCODE_MASK) + if format == ESCAPE_FORMAT { 64 } else { 0 };
        // SAFETY: verification rejected every base or extended opcode outside `Opcode`.
        let opcode = unsafe { core::mem::transmute::<u8, Opcode>(semantic_opcode) };
        let operand_count = opcode.operand_count();
        let mut operands = [0; 3];

        if format == 0 {
            decode_compact_operands(header, operand_count, &mut operands);
            return decoded_instruction(opcode, OperandWidth::Compact, operands, operand_count, 1);
        }
        if format == NORMAL_FORMAT {
            // SAFETY: verification proved the packed operand words are present at this boundary.
            unsafe { self.decode_normal_operands(start, operand_count, &mut operands) };
            return decoded_instruction(
                opcode,
                OperandWidth::Normal,
                operands,
                operand_count,
                1 + operand_count.div_ceil(2),
            );
        }
        // Wide base instructions and escaped extended instructions both store full `u32` operands.
        // SAFETY: verification proved all `operand_count` words are present at this boundary.
        unsafe { self.decode_full_width_operands(start, operand_count, &mut operands) };
        decoded_instruction(
            opcode,
            OperandWidth::Wide,
            operands,
            operand_count,
            1 + operand_count,
        )
    }

    /// Restores up to three packed `u16` operands from verifier-proven backing words.
    #[inline(always)]
    unsafe fn decode_normal_operands(
        &self,
        start: usize,
        operand_count: usize,
        operands: &mut [u32; 3],
    ) {
        if operand_count != 0 {
            // SAFETY: the caller guarantees the complete normal instruction is in `self.words`.
            let first = unsafe { *self.words.get_unchecked(start + 1) };
            operands[0] = first & 0xffff;
            if operand_count > 1 {
                operands[1] = first >> 16;
            }
        }
        if operand_count > 2 {
            // SAFETY: three-operand normal instructions contain a second packed operand word.
            operands[2] = unsafe { *self.words.get_unchecked(start + 2) } & 0xffff;
        }
    }

    /// Restores up to three full-width operands from verifier-proven backing words.
    #[inline(always)]
    unsafe fn decode_full_width_operands(
        &self,
        start: usize,
        operand_count: usize,
        operands: &mut [u32; 3],
    ) {
        if operand_count > 0 {
            // SAFETY: the caller guarantees the complete wide/escape instruction is in `self.words`.
            operands[0] = unsafe { *self.words.get_unchecked(start + 1) };
        }
        if operand_count > 1 {
            // SAFETY: two-operand wide/escape instructions contain this word.
            operands[1] = unsafe { *self.words.get_unchecked(start + 2) };
        }
        if operand_count > 2 {
            // SAFETY: three-operand wide/escape instructions contain this word.
            operands[2] = unsafe { *self.words.get_unchecked(start + 3) };
        }
    }
}

#[inline(always)]
fn decode_compact_operands(header: u32, operand_count: usize, operands: &mut [u32; 3]) {
    if operand_count > 0 {
        operands[0] = (header >> 8) & 0xff;
    }
    if operand_count > 1 {
        operands[1] = (header >> 16) & 0xff;
    }
    if operand_count > 2 {
        operands[2] = header >> 24;
    }
}

#[inline(always)]
fn decoded_instruction(
    opcode: Opcode,
    width: OperandWidth,
    operands: [u32; 3],
    operand_count: usize,
    word_len: usize,
) -> DecodedInstruction {
    DecodedInstruction {
        opcode,
        width,
        operands,
        operand_count: operand_count as u8,
        word_len: word_len as u8,
    }
}

/// An immutable constant-pool entry that is independent from every isolate and runtime heap.
///
/// Strings use owned UTF-16 code units so literals containing lone surrogate code points retain
/// their ECMAScript representation without requiring a runtime string allocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BytecodeConstant {
    NumberBits(u64),
    String(Arc<[u16]>),
    BigInt(Arc<str>),
    RegExp { pattern: Arc<[u16]>, flags: u8 },
}

impl BytecodeConstant {
    #[must_use]
    pub fn string_from_utf16(code_units: Vec<u16>) -> Self {
        Self::String(code_units.into())
    }

    #[must_use]
    pub fn regexp_from_utf16(pattern: Vec<u16>, flags: u8) -> Self {
        Self::RegExp {
            pattern: pattern.into(),
            flags,
        }
    }
}

/// An exception handler range, expressed exclusively as word offsets in one function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandlerEntry {
    pub protected_start: WordOffset,
    pub protected_end: WordOffset,
    pub handler: WordOffset,
    pub kind: HandlerKind,
    pub environment_depth: u32,
}

/// The distinct unwind behavior required when an abrupt completion reaches a protected range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandlerKind {
    Catch,
    Finally,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FunctionKind {
    Script,
    Module,
    Ordinary,
    Generator,
    Async,
    AsyncGenerator,
}

/// Immutable source-level strictness used by call binding and strict-only runtime semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FunctionStrictness {
    Sloppy,
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct SuspendPointId(u32);

impl SuspendPointId {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// The immutable compiler-side contract for resuming a suspended generator or async fiber.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SuspendPoint {
    pub id: SuspendPointId,
    pub instruction: WordOffset,
    pub resume_offset: WordOffset,
    pub destination: RegisterId,
    pub completion_depth: u32,
}

/// The immutable location of isolate-local feedback; the feedback data itself never enters code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeedbackSite {
    pub offset: WordOffset,
    pub slot: FeedbackSlot,
}

/// The concrete runtime storage class selected for one source binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingLocation {
    FrameRegister(RegisterId),
    Environment { depth: u32, slot: u32 },
    ModuleCell { slot: u32 },
    GlobalLexical,
    GlobalProperty,
    Dynamic,
}

/// Immutable source-name, mutability, and storage metadata for one binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingPlanEntry {
    pub name: Arc<str>,
    pub location: BindingLocation,
    pub mutable: bool,
}

/// Register and stack-reservation requirements known before a function begins execution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FunctionLayout {
    pub register_count: u32,
    pub argument_count: u32,
    /// Number of parameters before the first default/rest parameter for the `length` property.
    pub function_length: u32,
    /// Optional module scope-name index used by the virtual `name` property.
    pub name_scope: Option<u32>,
    pub temporary_register_count: u32,
    pub feedback_slot_count: u32,
    pub environment_slot_count: u32,
    pub max_handler_depth: u32,
    pub max_completion_depth: u32,
}

/// Per-function metadata which the compiler owns until `CompiledModule::new` verifies and freezes it.
#[derive(Clone, Debug)]
pub struct FunctionMetadata {
    pub kind: FunctionKind,
    pub strictness: FunctionStrictness,
    pub layout: FunctionLayout,
    pub source_map: Arc<[SourceMapEntry]>,
    pub handlers: Arc<[HandlerEntry]>,
    pub suspend_points: Arc<[SuspendPoint]>,
    pub feedback_sites: Arc<[FeedbackSite]>,
    pub binding_plan: Arc<[BindingPlanEntry]>,
}

impl FunctionMetadata {
    #[must_use]
    pub fn new(kind: FunctionKind, layout: FunctionLayout) -> Self {
        let strictness = if matches!(kind, FunctionKind::Module) {
            FunctionStrictness::Strict
        } else {
            FunctionStrictness::Sloppy
        };
        Self {
            kind,
            strictness,
            layout,
            source_map: Arc::default(),
            handlers: Arc::default(),
            suspend_points: Arc::default(),
            feedback_sites: Arc::default(),
            binding_plan: Arc::default(),
        }
    }
}

/// Mutable-at-construction data that the compiler submits for module verification and freezing.
#[derive(Clone, Debug)]
pub struct CompiledFunctionTemplate {
    id: FunctionId,
    bytecode: Bytecode,
    metadata: FunctionMetadata,
}

impl CompiledFunctionTemplate {
    /// Creates an unverified compiler output; `CompiledModule::new` owns all validation and freezing.
    #[must_use]
    pub fn new(id: FunctionId, bytecode: Bytecode, metadata: FunctionMetadata) -> Self {
        Self {
            id,
            bytecode,
            metadata,
        }
    }
}

/// A verified function whose code and metadata can be shared across isolates.
#[derive(Clone, Debug)]
pub struct CompiledFunction {
    id: FunctionId,
    bytecode: VerifiedBytecode,
    metadata: FunctionMetadata,
}

impl CompiledFunction {
    #[must_use]
    pub const fn id(&self) -> FunctionId {
        self.id
    }

    #[must_use]
    pub fn bytecode(&self) -> &VerifiedBytecode {
        &self.bytecode
    }

    #[must_use]
    pub const fn layout(&self) -> FunctionLayout {
        self.metadata.layout
    }

    #[must_use]
    pub const fn kind(&self) -> FunctionKind {
        self.metadata.kind
    }

    #[must_use]
    pub const fn strictness(&self) -> FunctionStrictness {
        self.metadata.strictness
    }

    #[must_use]
    pub fn source_map(&self) -> &[SourceMapEntry] {
        &self.metadata.source_map
    }

    #[must_use]
    pub fn handlers(&self) -> &[HandlerEntry] {
        &self.metadata.handlers
    }

    #[must_use]
    pub fn suspend_points(&self) -> &[SuspendPoint] {
        &self.metadata.suspend_points
    }

    #[must_use]
    pub fn feedback_sites(&self) -> &[FeedbackSite] {
        &self.metadata.feedback_sites
    }

    #[must_use]
    pub fn binding_plan(&self) -> &[BindingPlanEntry] {
        &self.metadata.binding_plan
    }
}

/// A fully verified, immutable compilation result with no isolate-local or runtime-heap state.
#[derive(Clone, Debug)]
pub struct CompiledModule {
    source: Arc<str>,
    constants: Arc<[BytecodeConstant]>,
    scope_names: Arc<[Arc<str>]>,
    functions: Arc<[CompiledFunction]>,
    entry_function: FunctionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleBuildError {
    SourceTooLarge {
        byte_len: usize,
    },
    TooManyConstants {
        count: usize,
    },
    TooManyScopeNames {
        count: usize,
    },
    TooManyFunctions {
        count: usize,
    },
    FunctionIdMismatch {
        expected: FunctionId,
        actual: FunctionId,
    },
    InvalidEntryFunction {
        entry: FunctionId,
        function_count: u32,
    },
    InvalidFunctionLayout {
        function: FunctionId,
        layout: FunctionLayout,
    },
    InvalidFunctionStrictness {
        function: FunctionId,
        kind: FunctionKind,
        strictness: FunctionStrictness,
    },
    VerifyFunction {
        function: FunctionId,
        error: VerifyError,
    },
    SourceMapOffsetNotInstructionStart {
        function: FunctionId,
        offset: WordOffset,
    },
    SourceMapOutOfBounds {
        function: FunctionId,
        span: SourceSpan,
        source_len: u32,
    },
    SourceMapNotMonotonic {
        function: FunctionId,
        previous: WordOffset,
        current: WordOffset,
    },
    InvalidHandlerRange {
        function: FunctionId,
        handler: HandlerEntry,
    },
    HandlerTargetInsideProtectedRange {
        function: FunctionId,
        handler: HandlerEntry,
    },
    HandlerTableNotProperlyNested {
        function: FunctionId,
        previous: HandlerEntry,
        current: HandlerEntry,
    },
    FeedbackSiteOffsetNotInstructionStart {
        function: FunctionId,
        offset: WordOffset,
    },
    FeedbackSlotOutOfRange {
        function: FunctionId,
        slot: FeedbackSlot,
        feedback_slot_count: u32,
    },
    FeedbackSitesNotMonotonic {
        function: FunctionId,
        previous: WordOffset,
        current: WordOffset,
    },
    BindingRegisterOutOfRange {
        function: FunctionId,
        binding: BindingPlanEntry,
        register_count: u32,
    },
    BindingEnvironmentSlotOutOfRange {
        function: FunctionId,
        binding: BindingPlanEntry,
        environment_slot_count: u32,
    },
    EmptyBindingName {
        function: FunctionId,
        binding: BindingPlanEntry,
    },
    SuspendPointIdMismatch {
        function: FunctionId,
        expected: SuspendPointId,
        actual: SuspendPointId,
    },
    InvalidSuspendPoint {
        function: FunctionId,
        suspend_point: SuspendPoint,
    },
    SuspendPointMissing {
        function: FunctionId,
        offset: WordOffset,
        id: SuspendPointId,
    },
    SuspendInIncompatibleFunction {
        function: FunctionId,
        kind: FunctionKind,
        offset: WordOffset,
        opcode: Opcode,
    },
    VerifiedBytecodeDecodeInvariant {
        function: FunctionId,
        offset: WordOffset,
    },
}

impl CompiledModule {
    /// Verifies every function against this module's pool sizes before freezing all data into shared slices.
    pub fn new(
        source: Arc<str>,
        constants: Vec<BytecodeConstant>,
        scope_names: Vec<Arc<str>>,
        templates: Vec<CompiledFunctionTemplate>,
        entry_function: FunctionId,
    ) -> Result<Self, ModuleBuildError> {
        let source_len =
            u32::try_from(source.len()).map_err(|_| ModuleBuildError::SourceTooLarge {
                byte_len: source.len(),
            })?;
        let constant_count =
            u32::try_from(constants.len()).map_err(|_| ModuleBuildError::TooManyConstants {
                count: constants.len(),
            })?;
        let scope_name_count =
            u32::try_from(scope_names.len()).map_err(|_| ModuleBuildError::TooManyScopeNames {
                count: scope_names.len(),
            })?;
        let function_count =
            u32::try_from(templates.len()).map_err(|_| ModuleBuildError::TooManyFunctions {
                count: templates.len(),
            })?;
        if entry_function.index() >= function_count {
            return Err(ModuleBuildError::InvalidEntryFunction {
                entry: entry_function,
                function_count,
            });
        }

        let max_environment_slot_count = templates
            .iter()
            .map(|template| template.metadata.layout.environment_slot_count)
            .max()
            .unwrap_or(0);
        let context = VerifyContext {
            register_count: 0,
            constant_count,
            function_count,
            scope_name_count,
            max_environment_slot_count,
        };
        let mut functions = Vec::with_capacity(templates.len());
        for (index, template) in templates.into_iter().enumerate() {
            let expected = FunctionId::new(index as u32);
            if template.id != expected {
                return Err(ModuleBuildError::FunctionIdMismatch {
                    expected,
                    actual: template.id,
                });
            }
            if matches!(template.metadata.kind, FunctionKind::Module)
                && template.metadata.strictness != FunctionStrictness::Strict
            {
                return Err(ModuleBuildError::InvalidFunctionStrictness {
                    function: template.id,
                    kind: template.metadata.kind,
                    strictness: template.metadata.strictness,
                });
            }
            let bytecode = template
                .bytecode
                .verify(VerifyContext {
                    register_count: template.metadata.layout.register_count,
                    ..context
                })
                .map_err(|error| ModuleBuildError::VerifyFunction {
                    function: template.id,
                    error,
                })?;
            validate_source_map(
                template.id,
                &template.metadata.source_map,
                &bytecode,
                source_len,
            )?;
            let handler_depth =
                validate_handlers(template.id, &template.metadata.handlers, &bytecode)?;
            validate_function_layout(
                template.id,
                template.metadata.layout,
                handler_depth,
                scope_name_count,
            )?;
            validate_feedback_sites(
                template.id,
                &template.metadata.feedback_sites,
                &bytecode,
                template.metadata.layout.feedback_slot_count,
            )?;
            validate_binding_plan(
                template.id,
                &template.metadata.binding_plan,
                template.metadata.layout,
                max_environment_slot_count,
            )?;
            validate_suspend_points(
                template.id,
                &template.metadata.suspend_points,
                &bytecode,
                template.metadata.layout.register_count,
                template.metadata.kind,
            )?;
            functions.push(CompiledFunction {
                id: template.id,
                bytecode,
                metadata: template.metadata,
            });
        }
        Ok(Self {
            source,
            constants: constants.into(),
            scope_names: scope_names.into(),
            functions: functions.into(),
            entry_function,
        })
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn constants(&self) -> &[BytecodeConstant] {
        &self.constants
    }

    #[must_use]
    pub fn scope_names(&self) -> &[Arc<str>] {
        &self.scope_names
    }

    /// Compares immutable backing identity without hashing source or bytecode on every isolate entry.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.functions, &other.functions)
            && Arc::ptr_eq(&self.constants, &other.constants)
            && Arc::ptr_eq(&self.scope_names, &other.scope_names)
    }

    #[must_use]
    pub fn function(&self, id: FunctionId) -> Option<&CompiledFunction> {
        self.functions.get(id.index() as usize)
    }

    #[must_use]
    pub fn functions(&self) -> &[CompiledFunction] {
        &self.functions
    }

    #[must_use]
    pub const fn entry_function(&self) -> FunctionId {
        self.entry_function
    }
}

/// Layout metadata must permit the VM to reserve all function-local windows before dispatch starts.
fn validate_function_layout(
    function: FunctionId,
    layout: FunctionLayout,
    handler_depth: HandlerDepth,
    scope_name_count: u32,
) -> Result<(), ModuleBuildError> {
    if layout.argument_count > layout.register_count
        || layout.temporary_register_count > layout.register_count - layout.argument_count
        || layout.max_handler_depth < handler_depth.handlers
        || layout.max_completion_depth < handler_depth.finally_handlers
        || layout
            .name_scope
            .is_some_and(|name_scope| name_scope >= scope_name_count)
    {
        return Err(ModuleBuildError::InvalidFunctionLayout { function, layout });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct HandlerDepth {
    handlers: u32,
    finally_handlers: u32,
}

/// Source maps may omit instructions but each retained entry must be an ordered valid instruction start.
fn validate_source_map(
    function: FunctionId,
    source_map: &[SourceMapEntry],
    bytecode: &VerifiedBytecode,
    source_len: u32,
) -> Result<(), ModuleBuildError> {
    let mut previous: Option<WordOffset> = None;
    for entry in source_map {
        if !bytecode.is_instruction_start(entry.offset) {
            return Err(ModuleBuildError::SourceMapOffsetNotInstructionStart {
                function,
                offset: entry.offset,
            });
        }
        if entry.span.start > entry.span.end || entry.span.end > source_len {
            return Err(ModuleBuildError::SourceMapOutOfBounds {
                function,
                span: entry.span,
                source_len,
            });
        }
        if let Some(previous) = previous.filter(|previous| entry.offset.index() <= previous.index())
        {
            return Err(ModuleBuildError::SourceMapNotMonotonic {
                function,
                previous,
                current: entry.offset,
            });
        }
        previous = Some(entry.offset);
    }
    Ok(())
}

/// Handler ranges are half-open and must refer only to decoded instruction boundaries in their function.
fn validate_handlers(
    function: FunctionId,
    handlers: &[HandlerEntry],
    bytecode: &VerifiedBytecode,
) -> Result<HandlerDepth, ModuleBuildError> {
    let code_len = bytecode.bytecode().words().len() as u32;
    for &handler in handlers {
        let start_is_valid = bytecode.is_instruction_start(handler.protected_start);
        let end_is_valid = handler.protected_end.index() == code_len
            || bytecode.is_instruction_start(handler.protected_end);
        if !start_is_valid
            || !end_is_valid
            || !bytecode.is_instruction_start(handler.handler)
            || handler.protected_start.index() >= handler.protected_end.index()
        {
            return Err(ModuleBuildError::InvalidHandlerRange { function, handler });
        }
        if handler.protected_start.index() <= handler.handler.index()
            && handler.handler.index() < handler.protected_end.index()
        {
            return Err(ModuleBuildError::HandlerTargetInsideProtectedRange { function, handler });
        }
    }
    let mut max_depth = HandlerDepth::default();
    for (index, &current) in handlers.iter().enumerate() {
        for &previous in &handlers[..index] {
            if previous.protected_start.index() > current.protected_start.index()
                || (previous.protected_start == current.protected_start
                    && previous.protected_end.index() <= current.protected_end.index())
                || crosses_handler_ranges(previous, current)
            {
                return Err(ModuleBuildError::HandlerTableNotProperlyNested {
                    function,
                    previous,
                    current,
                });
            }
        }
        let mut depth = 0u32;
        let mut finally_depth = 0u32;
        for &candidate in &handlers[..=index] {
            if handler_contains(candidate, current) {
                depth += 1;
                if candidate.kind == HandlerKind::Finally {
                    finally_depth += 1;
                }
            }
        }
        max_depth.handlers = max_depth.handlers.max(depth);
        max_depth.finally_handlers = max_depth.finally_handlers.max(finally_depth);
    }
    Ok(max_depth)
}

fn handler_contains(outer: HandlerEntry, inner: HandlerEntry) -> bool {
    outer.protected_start.index() <= inner.protected_start.index()
        && inner.protected_end.index() <= outer.protected_end.index()
}

fn crosses_handler_ranges(left: HandlerEntry, right: HandlerEntry) -> bool {
    left.protected_start.index() < right.protected_start.index()
        && right.protected_start.index() < left.protected_end.index()
        && left.protected_end.index() < right.protected_end.index()
}

/// Feedback sites have stable ordering and bounds, while their mutable feedback stays isolate-local.
fn validate_feedback_sites(
    function: FunctionId,
    feedback_sites: &[FeedbackSite],
    bytecode: &VerifiedBytecode,
    feedback_slot_count: u32,
) -> Result<(), ModuleBuildError> {
    let mut previous: Option<WordOffset> = None;
    for &site in feedback_sites {
        if !bytecode.is_instruction_start(site.offset) {
            return Err(ModuleBuildError::FeedbackSiteOffsetNotInstructionStart {
                function,
                offset: site.offset,
            });
        }
        if site.slot.index() >= feedback_slot_count {
            return Err(ModuleBuildError::FeedbackSlotOutOfRange {
                function,
                slot: site.slot,
                feedback_slot_count,
            });
        }
        if let Some(previous) = previous.filter(|previous| site.offset.index() <= previous.index())
        {
            return Err(ModuleBuildError::FeedbackSitesNotMonotonic {
                function,
                previous,
                current: site.offset,
            });
        }
        previous = Some(site.offset);
    }
    Ok(())
}

/// Binding plans may name future storage classes, but every currently bounded index is verified.
fn validate_binding_plan(
    function: FunctionId,
    bindings: &[BindingPlanEntry],
    layout: FunctionLayout,
    max_environment_slot_count: u32,
) -> Result<(), ModuleBuildError> {
    for binding in bindings {
        if binding.name.is_empty() {
            return Err(ModuleBuildError::EmptyBindingName {
                function,
                binding: binding.clone(),
            });
        }
        match binding.location {
            BindingLocation::FrameRegister(register)
                if register.index() >= layout.register_count =>
            {
                return Err(ModuleBuildError::BindingRegisterOutOfRange {
                    function,
                    binding: binding.clone(),
                    register_count: layout.register_count,
                });
            }
            BindingLocation::Environment { slot, .. } if slot >= max_environment_slot_count => {
                return Err(ModuleBuildError::BindingEnvironmentSlotOutOfRange {
                    function,
                    binding: binding.clone(),
                    environment_slot_count: max_environment_slot_count,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Suspend metadata gives a resumed fiber enough information to restore state without replaying bytecode.
fn validate_suspend_points(
    function: FunctionId,
    suspend_points: &[SuspendPoint],
    bytecode: &VerifiedBytecode,
    register_count: u32,
    kind: FunctionKind,
) -> Result<(), ModuleBuildError> {
    for (index, &suspend_point) in suspend_points.iter().enumerate() {
        let expected = SuspendPointId::new(u32::try_from(index).map_err(|_| {
            ModuleBuildError::InvalidSuspendPoint {
                function,
                suspend_point,
            }
        })?);
        if suspend_point.id != expected {
            return Err(ModuleBuildError::SuspendPointIdMismatch {
                function,
                expected,
                actual: suspend_point.id,
            });
        }
        if !bytecode.is_instruction_start(suspend_point.instruction)
            || !bytecode.is_instruction_start(suspend_point.resume_offset)
            || suspend_point.destination.index() >= register_count
        {
            return Err(ModuleBuildError::InvalidSuspendPoint {
                function,
                suspend_point,
            });
        }
        let instruction =
            decode_instruction(bytecode.bytecode().words(), suspend_point.instruction).map_err(
                |_| ModuleBuildError::VerifiedBytecodeDecodeInvariant {
                    function,
                    offset: suspend_point.instruction,
                },
            )?;
        if !matches!(instruction.opcode, Opcode::Await | Opcode::Yield)
            || instruction.operands[1] != suspend_point.destination.index()
            || instruction.operands[2] != suspend_point.id.index()
            || suspend_point.resume_offset.index()
                != suspend_point.instruction.index() + u32::from(instruction.word_len)
        {
            return Err(ModuleBuildError::InvalidSuspendPoint {
                function,
                suspend_point,
            });
        }
    }
    validate_suspend_opcodes(function, kind, bytecode, suspend_points)
}

/// Every suspend opcode must name metadata with a compatible function kind so resume is deterministic.
fn validate_suspend_opcodes(
    function: FunctionId,
    kind: FunctionKind,
    bytecode: &VerifiedBytecode,
    suspend_points: &[SuspendPoint],
) -> Result<(), ModuleBuildError> {
    let words = bytecode.bytecode().words();
    let mut offset = 0u32;
    while (offset as usize) < words.len() {
        let word_offset = WordOffset::new(offset);
        let decoded = decode_instruction(words, word_offset).map_err(|_| {
            ModuleBuildError::VerifiedBytecodeDecodeInvariant {
                function,
                offset: word_offset,
            }
        })?;
        if matches!(decoded.opcode, Opcode::Await | Opcode::Yield) {
            let is_compatible = match decoded.opcode {
                Opcode::Await => matches!(
                    kind,
                    FunctionKind::Module | FunctionKind::Async | FunctionKind::AsyncGenerator
                ),
                Opcode::Yield => {
                    matches!(kind, FunctionKind::Generator | FunctionKind::AsyncGenerator)
                }
                _ => false,
            };
            if !is_compatible {
                return Err(ModuleBuildError::SuspendInIncompatibleFunction {
                    function,
                    kind,
                    offset: word_offset,
                    opcode: decoded.opcode,
                });
            }
            let id = SuspendPointId::new(decoded.operands[2]);
            if suspend_points
                .get(id.index() as usize)
                .map(|point| point.id)
                != Some(id)
            {
                return Err(ModuleBuildError::SuspendPointMissing {
                    function,
                    offset: word_offset,
                    id,
                });
            }
        }
        offset += u32::from(decoded.word_len);
    }
    Ok(())
}

/// Verifies the complete stream in two passes so jumps can only land on decoded instruction boundaries.
fn verify(bytecode: Bytecode, context: VerifyContext) -> Result<VerifiedBytecode, VerifyError> {
    let words = bytecode.words();
    if words.len() > u32::MAX as usize {
        return Err(VerifyError::CodeTooLarge {
            word_count: words.len(),
        });
    }
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
        Opcode::Nop
        | Opcode::Jump
        | Opcode::ReturnUndefined
        | Opcode::DeclareScope
        | Opcode::DeclareGlobalLexical => {}
        Opcode::LoadUndefined
        | Opcode::LoadNull
        | Opcode::LoadFalse
        | Opcode::LoadTrue
        | Opcode::LoadImmediate
        | Opcode::LoadConstant
        | Opcode::LoadScope => check_register(operands[0])?,
        Opcode::CreateObject
        | Opcode::CreateArray
        | Opcode::LoadException
        | Opcode::LoadThis
        | Opcode::LoadNewTarget
        | Opcode::LoadArgumentsLength => check_register(operands[0])?,
        Opcode::Move => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::Not
        | Opcode::Negate
        | Opcode::Typeof
        | Opcode::ToNumber
        | Opcode::BitwiseNot
        | Opcode::TypeofScope
        | Opcode::CreateForInIterator
        | Opcode::ForInNext => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
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
        | Opcode::SetByValue => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            check_register(operands[2])?;
        }
        Opcode::LoadEnvironment | Opcode::StoreEnvironment => check_register(operands[0])?,
        Opcode::GetById | Opcode::SetById => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::Call | Opcode::Construct => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            if operands[1]
                .checked_add(operands[2])
                .is_none_or(|last_argument| last_argument >= context.register_count)
                && operands[2] != 0
            {
                return Err(VerifyError::InvalidCallArgumentWindow {
                    offset,
                    callee: operands[1],
                    argument_count: operands[2],
                    register_count: context.register_count,
                });
            }
        }
        Opcode::CallWithReceiver => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            if operands[1]
                .checked_add(1)
                .and_then(|callee| callee.checked_add(operands[2]))
                .is_none_or(|last_argument| last_argument >= context.register_count)
            {
                return Err(VerifyError::InvalidCallArgumentWindow {
                    offset,
                    callee: operands[1].saturating_add(1),
                    argument_count: operands[2],
                    register_count: context.register_count,
                });
            }
        }
        Opcode::Await | Opcode::Yield => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish => {
            check_register(operands[0])?
        }
        Opcode::Return | Opcode::Throw => check_register(operands[0])?,
        Opcode::CreateClosure => check_register(operands[0])?,
        Opcode::StoreScope | Opcode::StoreResolvedScope | Opcode::InitializeGlobalLexical => {
            check_register(operands[0])?
        }
    }
    if instruction.opcode == Opcode::LoadConstant && operands[1] >= context.constant_count {
        return Err(VerifyError::ConstantOutOfRange {
            offset,
            constant: operands[1],
            constant_count: context.constant_count,
        });
    }
    let scope_name = match instruction.opcode {
        Opcode::LoadScope | Opcode::StoreScope | Opcode::StoreResolvedScope => Some(operands[1]),
        Opcode::DeclareScope => Some(operands[0]),
        Opcode::DeclareGlobalLexical => Some(operands[0]),
        Opcode::InitializeGlobalLexical => Some(operands[1]),
        Opcode::GetById | Opcode::SetById => Some(operands[2]),
        _ => None,
    };
    if scope_name.is_some_and(|scope_name| scope_name >= context.scope_name_count) {
        return Err(VerifyError::ScopeNameOutOfRange {
            offset,
            scope_name: scope_name.expect("scope-name opcode selected above"),
            scope_name_count: context.scope_name_count,
        });
    }
    if instruction.opcode == Opcode::CreateClosure && operands[1] >= context.function_count {
        return Err(VerifyError::FunctionOutOfRange {
            offset,
            function: operands[1],
            function_count: context.function_count,
        });
    }
    if matches!(
        instruction.opcode,
        Opcode::LoadEnvironment | Opcode::StoreEnvironment
    ) && operands[2] >= context.max_environment_slot_count
    {
        return Err(VerifyError::EnvironmentSlotOutOfRange {
            offset,
            slot: operands[2],
            max_environment_slot_count: context.max_environment_slot_count,
        });
    }
    if instruction.opcode == Opcode::DeclareGlobalLexical && operands[1] > 1 {
        return Err(VerifyError::InvalidBooleanOperand {
            offset,
            operand: operands[1],
        });
    }
    if matches!(
        instruction.opcode,
        Opcode::Jump | Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish
    ) && !starts
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
mod tests;
