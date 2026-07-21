//! Logical bytecode operands, opcode metadata, and checked word encodings.

use core::fmt;

pub(super) const OPCODE_MASK: u8 = 0x3f;
const BASE_OPCODE_COUNT: usize = OPCODE_MASK as usize + 1;
pub(super) const FORMAT_MASK: u8 = 0xc0;
pub(super) const ESCAPE_FORMAT: u8 = 0xc0;
pub(super) const NORMAL_FORMAT: u8 = 0x40;
pub(super) const WIDE_FORMAT: u8 = 0x80;

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
    /// Saves a normal completion and enters the innermost covering finalizer.
    EnterFinally = 70,
    /// Restores and redispatches the completion owned by the active finalizer.
    ResumeCompletion = 71,
    /// Transfers a break completion through finalizers crossed by its target.
    BreakThroughFinally = 72,
    /// Transfers a continue completion through finalizers crossed by its target.
    ContinueThroughFinally = 73,
    /// Prepares a computed key after checking that its base is coercible.
    ToPropertyKey = 74,
    /// Prepares an `in` key only after confirming that its right operand is an Object.
    ToPropertyKeyForIn = 75,
    /// Assigns an inferred name to a newly-created anonymous function.
    SetFunctionName = 76,
    /// Defines or updates the getter half of an object literal own accessor property.
    DefineGetterById = 77,
    /// Defines or updates the setter half of an object literal own accessor property.
    DefineSetterById = 78,
    /// Defines or updates the getter half of an object literal with a runtime PropertyKey.
    DefineGetterByValue = 79,
    /// Defines or updates the setter half of an object literal with a runtime PropertyKey.
    DefineSetterByValue = 80,
    /// Assigns an accessor's `get` or `set` name from an already-normalized PropertyKey value.
    SetAccessorFunctionName = 81,
    /// Allocates one VM-private exact-capacity object-rest exclusion list.
    CreateExclusionList = 82,
    /// Appends one normalized PropertyKey to a VM-private exclusion list.
    ExcludePropertyKey = 83,
    /// Copies enumerable own properties while omitting keys held by a VM-private exclusion list.
    CopyDataProperties = 84,
    /// Collects the active function's positional arguments starting at one fixed parameter index.
    CollectRestArguments = 85,
    /// Materializes the active function's actual arguments as one independent Array-like object.
    LoadArgumentsObject = 86,
    /// Creates one derived class constructor from a function template and evaluated superclass.
    CreateClass = 87,
    /// Constructs the active derived class's superclass with the current `new.target`.
    SuperConstruct = 88,
    /// Publishes a completed `super()` result as the active derived constructor's `this` value.
    InitializeThis = 89,
    /// Validates class heritage before its observable `prototype` property access.
    CheckConstructor = 90,
}

pub(super) const OPCODE_COUNT: usize = 91;
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
    0, // EnterFinally
    0, // ResumeCompletion
    1, // BreakThroughFinally
    1, // ContinueThroughFinally
    3, // ToPropertyKey
    3, // ToPropertyKeyForIn
    2, // SetFunctionName
    3, // DefineGetterById
    3, // DefineSetterById
    3, // DefineGetterByValue
    3, // DefineSetterByValue
    3, // SetAccessorFunctionName
    2, // CreateExclusionList
    2, // ExcludePropertyKey
    3, // CopyDataProperties
    2, // CollectRestArguments
    1, // LoadArgumentsObject
    3, // CreateClass
    3, // SuperConstruct
    1, // InitializeThis
    1, // CheckConstructor
];

const _: [(); OPCODE_COUNT] = [(); OPCODE_OPERAND_COUNTS.len()];
const _: [(); OPCODE_COUNT] = [(); Opcode::CheckConstructor as usize + 1];

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
            Self::Jump
                | Self::Return
                | Self::ReturnUndefined
                | Self::Throw
                | Self::EnterFinally
                | Self::BreakThroughFinally
                | Self::ContinueThroughFinally
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
            6 => Some(Self::EnterFinally),
            7 => Some(Self::ResumeCompletion),
            8 => Some(Self::BreakThroughFinally),
            9 => Some(Self::ContinueThroughFinally),
            10 => Some(Self::ToPropertyKey),
            11 => Some(Self::ToPropertyKeyForIn),
            12 => Some(Self::SetFunctionName),
            13 => Some(Self::DefineGetterById),
            14 => Some(Self::DefineSetterById),
            15 => Some(Self::DefineGetterByValue),
            16 => Some(Self::DefineSetterByValue),
            17 => Some(Self::SetAccessorFunctionName),
            18 => Some(Self::CreateExclusionList),
            19 => Some(Self::ExcludePropertyKey),
            20 => Some(Self::CopyDataProperties),
            21 => Some(Self::CollectRestArguments),
            22 => Some(Self::LoadArgumentsObject),
            23 => Some(Self::CreateClass),
            24 => Some(Self::SuperConstruct),
            25 => Some(Self::InitializeThis),
            26 => Some(Self::CheckConstructor),
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

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
