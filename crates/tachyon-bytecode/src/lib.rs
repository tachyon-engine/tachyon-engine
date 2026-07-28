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

use std::sync::Arc;

use encoding::{ESCAPE_FORMAT, FORMAT_MASK, NORMAL_FORMAT, OPCODE_MASK, WIDE_FORMAT};

#[cfg(test)]
use encoding::OPCODE_COUNT;

mod builder;
mod disassembler;
mod encoding;
mod verify;

pub use builder::{BuilderError, BytecodeBuilder, Label, SourceMapEntry, SourceSpan};
pub use disassembler::{DisassemblyError, disassemble};
pub use encoding::{
    ConstantId, DecodeError, DecodedInstruction, EncodeError, FeedbackSlot, FunctionId,
    MAX_ENCODED_INSTRUCTION_WORDS, Opcode, OperandWidth, RegisterId, WordOffset,
    decode_instruction, encode_instruction,
};
pub use verify::{VerifyContext, VerifyError};

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
        verify::verify(self.clone(), context)
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
        let semantic_opcode = (raw & OPCODE_MASK)
            + if format == ESCAPE_FORMAT {
                64 + ((header >> 8) as u8) * 64
            } else {
                0
            };
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
    /// Exclusive finalizer end; catch handlers use `handler` as the sentinel.
    pub handler_end: WordOffset,
    pub kind: HandlerKind,
    pub environment_depth: u32,
}

/// The distinct unwind behavior required when an abrupt completion reaches a protected range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandlerKind {
    Catch,
    Finally,
    /// A finalizer that suppresses close errors while replaying an existing throw completion.
    IteratorClose,
}

impl HandlerKind {
    /// Returns whether this handler owns a saved completion and ends in `ResumeCompletion`.
    #[must_use]
    pub const fn is_finalizer(self) -> bool {
        matches!(self, Self::Finally | Self::IteratorClose)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FunctionKind {
    Script,
    Module,
    Ordinary,
    DerivedClassConstructor,
    BaseClassConstructor,
    ClassMethod,
    ClassFieldInitializer,
    Generator,
    Async,
    AsyncGenerator,
}

/// The immutable meaning of one constructor-owned instance-element record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ClassInstanceElementKind {
    PublicField = 0,
    PrivateField = 1,
    PrivateMethod = 2,
    PrivateAccessor = 3,
}

impl ClassInstanceElementKind {
    #[must_use]
    pub const fn from_operand(operand: u32) -> Option<Self> {
        match operand {
            0 => Some(Self::PublicField),
            1 => Some(Self::PrivateField),
            2 => Some(Self::PrivateMethod),
            3 => Some(Self::PrivateAccessor),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_private(self) -> bool {
        matches!(
            self,
            Self::PrivateField | Self::PrivateMethod | Self::PrivateAccessor
        )
    }
}

/// The runtime record category owning one dense environment-slot plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EnvironmentRecordKind {
    Declarative,
    Function,
    Global,
    Module,
}

impl EnvironmentRecordKind {
    #[must_use]
    pub const fn for_function_kind(kind: FunctionKind) -> Self {
        match kind {
            FunctionKind::Script => Self::Global,
            FunctionKind::Module => Self::Module,
            FunctionKind::Ordinary
            | FunctionKind::DerivedClassConstructor
            | FunctionKind::BaseClassConstructor
            | FunctionKind::ClassMethod
            | FunctionKind::ClassFieldInitializer
            | FunctionKind::Generator
            | FunctionKind::Async
            | FunctionKind::AsyncGenerator => Self::Function,
        }
    }
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
    Environment {
        depth: u32,
        slot: u32,
    },
    /// A dynamically-entered class-name environment rather than a function-owned slot.
    ClassEnvironment {
        depth: u32,
        slot: u32,
    },
    ModuleCell {
        slot: u32,
    },
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

/// Immutable owner metadata for one slot; its slice index is the dense runtime slot index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentSlotMetadata {
    pub name: Arc<str>,
    pub mutable: bool,
    /// Whether activation instantiation initializes this binding before bytecode begins.
    pub initialized: bool,
    /// Whether this slot belongs to a non-simple formal-parameter environment.
    pub parameter: bool,
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
    /// Immutable environment slot initialized to the called closure for a named function expression.
    pub self_binding_slot: Option<u32>,
    pub max_handler_depth: u32,
    pub max_completion_depth: u32,
    /// The function reads the original argument sequence after parameter initialization.
    pub needs_argument_source: bool,
    /// Whether a rest parameter makes the arguments object unmapped.
    pub has_rest_parameter: bool,
    /// Whether every formal parameter is a plain binding identifier.
    pub simple_parameter_list: bool,
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
    pub environment_record_kind: EnvironmentRecordKind,
    pub environment_slots: Arc<[EnvironmentSlotMetadata]>,
}

impl FunctionMetadata {
    #[must_use]
    pub fn new(kind: FunctionKind, layout: FunctionLayout) -> Self {
        let strictness = if matches!(
            kind,
            FunctionKind::Module
                | FunctionKind::DerivedClassConstructor
                | FunctionKind::BaseClassConstructor
                | FunctionKind::ClassMethod
                | FunctionKind::ClassFieldInitializer
        ) {
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
            environment_record_kind: EnvironmentRecordKind::for_function_kind(kind),
            environment_slots: Arc::default(),
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

    #[must_use]
    pub const fn environment_record_kind(&self) -> EnvironmentRecordKind {
        self.metadata.environment_record_kind
    }

    #[must_use]
    pub fn environment_slots(&self) -> &[EnvironmentSlotMetadata] {
        &self.metadata.environment_slots
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
    InvalidClassInstruction {
        function: FunctionId,
        kind: FunctionKind,
        offset: WordOffset,
        opcode: Opcode,
    },
    InvalidClassConstructorTarget {
        function: FunctionId,
        offset: WordOffset,
        target: FunctionId,
        target_kind: FunctionKind,
    },
    InvalidClassEnvironmentDepth {
        function: FunctionId,
        offset: WordOffset,
        expected: u32,
        actual: u32,
    },
    ClassEnvironmentSlotOutOfRange {
        function: FunctionId,
        offset: WordOffset,
        slot: u32,
        slot_count: u32,
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
    InvalidFinallyInstruction {
        function: FunctionId,
        offset: WordOffset,
        opcode: Opcode,
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
    EnvironmentSlotMetadataCountMismatch {
        function: FunctionId,
        expected: u32,
        actual: u32,
    },
    EnvironmentSlotBindingMismatch {
        function: FunctionId,
        binding: BindingPlanEntry,
    },
    EmptyEnvironmentSlotName {
        function: FunctionId,
        slot: u32,
    },
    InvalidSelfBindingSlot {
        function: FunctionId,
        slot: u32,
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

        let function_kinds: Vec<_> = templates
            .iter()
            .map(|template| template.metadata.kind)
            .collect();
        let encoded_class_environment_slots = templates
            .iter()
            .map(|template| {
                let mut offset = 0_u32;
                let mut maximum = 0_u32;
                while (offset as usize) < template.bytecode.words().len() {
                    let Ok(instruction) =
                        decode_instruction(template.bytecode.words(), WordOffset::new(offset))
                    else {
                        break;
                    };
                    if instruction.opcode == Opcode::EnterClassEnvironment {
                        maximum = maximum.max(instruction.operands[0]);
                    }
                    offset = offset.saturating_add(u32::from(instruction.word_len));
                }
                maximum
            })
            .max()
            .unwrap_or(0);
        let max_environment_slot_count = templates
            .iter()
            .map(|template| template.metadata.layout.environment_slot_count)
            .max()
            .unwrap_or(0)
            .max(encoded_class_environment_slots);
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
            if matches!(
                template.metadata.kind,
                FunctionKind::Module
                    | FunctionKind::DerivedClassConstructor
                    | FunctionKind::BaseClassConstructor
                    | FunctionKind::ClassMethod
                    | FunctionKind::ClassFieldInitializer
            ) && template.metadata.strictness != FunctionStrictness::Strict
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
            verify::validate_source_map(
                template.id,
                &template.metadata.source_map,
                &bytecode,
                source_len,
            )?;
            let handler_depth =
                verify::validate_handlers(template.id, &template.metadata.handlers, &bytecode)?;
            verify::validate_finally_instructions(
                template.id,
                &template.metadata.handlers,
                &bytecode,
            )?;
            verify::validate_class_instructions(
                template.id,
                template.metadata.kind,
                &bytecode,
                &function_kinds,
            )?;
            verify::validate_class_environments(
                template.id,
                &template.metadata.handlers,
                &bytecode,
            )?;
            verify::validate_function_layout(
                template.id,
                template.metadata.layout,
                handler_depth,
                scope_name_count,
            )?;
            verify::validate_feedback_sites(
                template.id,
                &template.metadata.feedback_sites,
                &bytecode,
                template.metadata.layout.feedback_slot_count,
            )?;
            verify::validate_binding_plan(
                template.id,
                &template.metadata.binding_plan,
                template.metadata.layout,
                max_environment_slot_count,
                &template.metadata.environment_slots,
            )?;
            verify::validate_environment_slots(
                template.id,
                &template.metadata.environment_slots,
                template.metadata.layout,
            )?;
            verify::validate_suspend_points(
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

#[cfg(test)]
mod tests;
