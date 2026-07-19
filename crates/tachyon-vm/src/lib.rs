#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Isolate, fiber, interpreter, and ECMAScript builtin execution machinery.
//!
//! This crate intentionally has no host I/O surface.

mod array;
mod atom;
mod bound_function;
mod builtins;
mod conversion;
#[cfg(feature = "opcode-profile")]
mod execution_profile;
mod finalization;
mod for_in;
mod interpreter;
mod number;
mod object;
mod realm;
mod string;
mod tuning;

pub use atom::{AtomHashSeed, AtomId, AtomTable, AtomTableConfig, AtomTableError, AtomTableStats};

#[cfg(feature = "opcode-profile")]
pub use execution_profile::{ExecutionProfile, OpcodeExecutionCounts};

pub use finalization::{
    FinalizationCleanupJob, FinalizationJobQueueStats, FinalizationSafepointError,
    FinalizationSafepointStats,
};
pub use object::ShapeError;
pub use string::{JsString, JsStringView, StringAllocationError, StringRepresentationTag};

use core::{cell::Cell, num::NonZeroU32, ptr::NonNull};

use tachyon_bytecode::{
    BytecodeConstant, CompiledModule, DecodedInstruction, FunctionId, FunctionKind, FunctionLayout,
    FunctionStrictness, HandlerEntry, HandlerKind, Opcode, RegisterId, VerifiedBytecode,
    VerifiedInstructionDecoder, WordOffset,
};
use tachyon_gc::{
    AllocationSpace, GcExternalMemory, GcRef, GcType, Heap, HeapAllocationError, HeapLimit,
    HeapReferenceError, ManagedAllocationError, NoGcBorrowError, RootError, Trace, Tracer,
    TypeRegistrationError, TypeRegistry,
};
use tachyon_value::{Immediate, Value};

use array::{ArrayObject, MAX_SAFE_INTEGER};
use bound_function::BoundFunctionData;
use conversion::{
    boolean_value, is_non_string_truthy, is_nullish, numeric_binary, numeric_binary_hot,
    numeric_binary_operation, numeric_bitwise_not, numeric_negate, numeric_relational,
    numeric_relational_hot, numeric_value, safe_integer_value, strict_equal_hot,
};
use for_in::{ForInAllocationError, ForInIterator, ForInKeySet};
use object::{
    NumberObject, OrdinaryObject, PropertyAttributes, PropertyLookup, PropertyStorage, ShapeId,
    ShapeTable,
};

/// Shareable immutable engine configuration. Host services deliberately do not live here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Engine;

/// Mandatory isolate resource and entropy configuration; production has no fixed hash seed.
/// Built-in ECMAScript Error families currently materialized by the realm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IsolateConfig {
    atom_table: AtomTableConfig,
    heap_limit: HeapLimit,
    stack_limits: StackLimits,
    realm_limits: RealmLimits,
}

impl IsolateConfig {
    #[must_use]
    pub const fn new(
        atom_table: AtomTableConfig,
        heap_limit: HeapLimit,
        stack_limits: StackLimits,
        realm_limits: RealmLimits,
    ) -> Self {
        Self {
            atom_table,
            heap_limit,
            stack_limits,
            realm_limits,
        }
    }
}

/// Host hard limits for isolate-retained code and global object bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealmLimits {
    max_loaded_modules: u32,
    max_global_bindings: u32,
    max_shapes: u32,
}

impl RealmLimits {
    #[must_use]
    pub const fn new(max_loaded_modules: u32, max_global_bindings: u32) -> Self {
        Self {
            max_loaded_modules,
            max_global_bindings,
            max_shapes: max_global_bindings,
        }
    }

    /// Overrides the hidden-class hard limit when object churn differs from global binding count.
    #[must_use]
    pub const fn with_max_shapes(mut self, max_shapes: u32) -> Self {
        self.max_shapes = max_shapes;
        self
    }
}

/// Host-provided hard bounds for explicit JavaScript frames and their register windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackLimits {
    max_frames: u32,
    max_registers: u32,
}

impl StackLimits {
    #[must_use]
    pub const fn new(max_frames: u32, max_registers: u32) -> Self {
        Self {
            max_frames,
            max_registers,
        }
    }
}

/// A per-execution bound; fuel is a hard cap while quantum bounds one synchronous interpreter turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionBudget {
    pub fuel: u64,
    pub quantum: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Completed(Value),
    Thrown(Value),
    BudgetExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    InvalidDispatchBatch { batch: usize },
    MissingEntryFunction(FunctionId),
    RegisterWindowTooLarge(u32),
    HandlerStackTooLarge(u32),
    CompletionStackTooLarge(u32),
    FrameAllocationFailed,
    RegisterAllocationFailed,
    EnvironmentStorageAllocationFailed,
    HandlerAllocationFailed,
    CompletionAllocationFailed,
    DecodeInvariant(WordOffset),
    UnsupportedOpcode(Opcode),
    UnsupportedConstant(u32),
    InvalidRegister(RegisterId),
    NonCallable(Value),
    NonConstructor(Value),
    InvalidInstanceofPrototype(Value),
    HeapAllocation(ManagedAllocationError),
    HeapReference(HeapReferenceError),
    Root(RootError),
    NoGcBorrow(NoGcBorrowError),
    MissingPendingException,
    MissingNativeContinuation,
    UnsupportedExceptionHandler(HandlerKind),
    CallStackLimit { limit: u32 },
    RegisterStackLimit { limit: u32, requested: u32 },
    LoadedModuleLimit { limit: u32 },
    LoadedCodeAllocationFailed,
    ScopeNameAllocationFailed,
    ScopeNameAtom(AtomTableError),
    ScopeNameString(StringAllocationError),
    ConstantValueAllocationFailed,
    ConstantString(StringAllocationError),
    PropertyKeyAtom(AtomTableError),
    PropertyKeyString(StringAllocationError),
    UnsupportedPropertyKey(Value),
    UnsupportedNumberConversion(Value),
    InvalidNumberRadix(Value),
    InvalidNumberPrecision(Value),
    NumberFormatBufferExhausted,
    NumberFormatInvalidDigit,
    NumberStringAllocationFailed,
    StringBufferAllocationFailed,
    UnsupportedTypeof(Value),
    InvalidCode(CodeId),
    InvalidScopeName { code: CodeId, scope_name: u32 },
    MissingEnvironment,
    InvalidEnvironmentSlot { depth: u32, slot: u32 },
    UnresolvedBinding(AtomId),
    ReadOnlyBinding(AtomId),
    UninitializedBinding(AtomId),
    ImmutableBinding(AtomId),
    GlobalLexicalRedeclaration(AtomId),
    GlobalLexicalAlreadyInitialized(AtomId),
    GlobalBindingLimit { limit: u32 },
    GlobalBindingAllocationFailed,
    GlobalBindingIndexAllocationFailed,
    IntrinsicBindingAllocationFailed,
    IntrinsicBindingIndexAllocationFailed,
    Shape(ShapeError),
    PropertyStorageAllocationFailed,
    BoundArgumentAllocationFailed,
    BoundArgumentCountOverflow,
    BoundNameAllocationFailed,
    ArrayLengthOverflow,
    ForInKeyAllocationFailed,
    InvalidForInIterator(Value),
    UnsupportedErrorMessage(Value),
    UnsupportedStringValue(Value),
    UnsupportedPrimitiveStringConversion(Value),
    UnsupportedDynamicFunctionConstructor,
    NonExtensibleObject(Value),
    ReadOnlyProperty(Value),
    InvalidPropertyRedefinition(Value),
    UnsupportedAccessorDescriptor,
    NotObject(Value),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IsolateCreationError {
    TypeRegistration(TypeRegistrationError),
    Shape(ShapeError),
    String(StringAllocationError),
    HeapAllocation(HeapAllocationError),
    IntrinsicInitialization(ExecutionError),
}

#[derive(Debug)]
struct Environment {
    parent: Option<GcRef<Environment>>,
    slots: Box<[Value]>,
}

impl Trace for Environment {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.parent.trace(tracer);
        self.slots.trace(tracer);
    }
}

impl GcExternalMemory for Environment {
    fn external_memory_bytes(&self) -> usize {
        self.slots.len() * core::mem::size_of::<Value>()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeFunction {
    ObjectConstructor,
    ObjectDefineProperty,
    ObjectGetOwnPropertyDescriptor,
    ObjectGetOwnPropertyNames,
    ObjectHasOwnProperty,
    ObjectPropertyIsEnumerable,
    ObjectToString,
    ObjectAssign,
    ObjectKeys,
    ObjectValues,
    ObjectEntries,
    ObjectHasOwn,
    ObjectIs,
    ObjectGetPrototypeOf,
    ObjectCreate,
    ObjectIsPrototypeOf,
    ObjectIsExtensible,
    ObjectPreventExtensions,
    StringConstructor,
    SymbolConstructor,
    NumberConstructor,
    NumberIsNaN,
    NumberIsFinite,
    NumberIsInteger,
    NumberIsSafeInteger,
    NumberToExponential,
    NumberToFixed,
    NumberToPrecision,
    NumberToString,
    NumberValueOf,
    BooleanConstructor,
    FunctionPrototype,
    FunctionPrototypeCall,
    FunctionPrototypeBind,
    FunctionConstructor,
    ErrorConstructor(NativeErrorKind),
    ArrayConstructor,
    ArrayIsArray,
    ArrayConcat,
    ArrayPush,
    ArrayJoin,
    ArrayAt,
    ArrayIndexOf,
    ArrayIncludes,
    ArrayPop,
    ArraySlice,
    ArrayShift,
    ArrayUnshift,
    ArrayReverse,
    ArrayFill,
    ArrayLastIndexOf,
    ArrayCopyWithin,
    ArrayFlat,
    ArraySort,
    ArrayToString,
    MathPow,
}

enum FlatWork {
    Value(Value, u32),
    Hole,
}

impl NativeFunction {
    #[inline(always)]
    const fn is_constructor(self) -> bool {
        matches!(
            self,
            Self::ObjectConstructor
                | Self::StringConstructor
                | Self::NumberConstructor
                | Self::BooleanConstructor
                | Self::FunctionConstructor
                | Self::ErrorConstructor(_)
                | Self::ArrayConstructor
        )
    }

    #[inline(always)]
    const fn length(self) -> i32 {
        match self {
            Self::ObjectDefineProperty => 3,
            Self::ObjectAssign
            | Self::ObjectHasOwn
            | Self::ObjectIs
            | Self::ObjectCreate
            | Self::ObjectGetOwnPropertyDescriptor => 2,
            Self::ObjectConstructor
            | Self::ObjectGetOwnPropertyNames
            | Self::ObjectHasOwnProperty
            | Self::ObjectPropertyIsEnumerable
            | Self::ObjectKeys
            | Self::ObjectValues
            | Self::ObjectEntries
            | Self::ObjectGetPrototypeOf
            | Self::ObjectIsPrototypeOf
            | Self::ObjectIsExtensible
            | Self::ObjectPreventExtensions
            | Self::StringConstructor
            | Self::NumberConstructor
            | Self::BooleanConstructor
            | Self::FunctionPrototypeCall
            | Self::FunctionPrototypeBind
            | Self::FunctionConstructor
            | Self::ErrorConstructor(_)
            | Self::ArrayConstructor
            | Self::ArrayIsArray
            | Self::ArrayConcat
            | Self::ArrayAt
            | Self::ArrayIndexOf
            | Self::ArrayIncludes
            | Self::ArrayPop
            | Self::ArraySlice
            | Self::ArrayShift
            | Self::ArrayUnshift
            | Self::ArrayReverse
            | Self::ArrayFill
            | Self::ArrayLastIndexOf
            | Self::ArrayCopyWithin
            | Self::ArrayFlat
            | Self::ArraySort => 1,
            Self::NumberIsNaN
            | Self::NumberIsFinite
            | Self::NumberIsInteger
            | Self::NumberIsSafeInteger
            | Self::NumberToExponential
            | Self::NumberToFixed
            | Self::NumberToPrecision
            | Self::NumberToString => 1,
            Self::ArrayPush | Self::ArrayJoin => 1,
            Self::MathPow => 2,
            Self::ObjectToString
            | Self::SymbolConstructor
            | Self::NumberValueOf
            | Self::FunctionPrototype
            | Self::ArrayToString => 0,
        }
    }

    #[inline]
    const fn name(self) -> &'static str {
        match self {
            Self::ObjectConstructor => "Object",
            Self::ObjectDefineProperty => "defineProperty",
            Self::ObjectGetOwnPropertyDescriptor => "getOwnPropertyDescriptor",
            Self::ObjectGetOwnPropertyNames => "getOwnPropertyNames",
            Self::ObjectHasOwnProperty => "hasOwnProperty",
            Self::ObjectPropertyIsEnumerable => "propertyIsEnumerable",
            Self::ObjectToString => "toString",
            Self::ObjectAssign => "assign",
            Self::ObjectKeys => "keys",
            Self::ObjectValues => "values",
            Self::ObjectEntries => "entries",
            Self::ObjectHasOwn => "hasOwn",
            Self::ObjectIs => "is",
            Self::ObjectGetPrototypeOf => "getPrototypeOf",
            Self::ObjectCreate => "create",
            Self::ObjectIsPrototypeOf => "isPrototypeOf",
            Self::ObjectIsExtensible => "isExtensible",
            Self::ObjectPreventExtensions => "preventExtensions",
            Self::StringConstructor => "String",
            Self::SymbolConstructor => "Symbol",
            Self::NumberConstructor => "Number",
            Self::NumberIsNaN => "isNaN",
            Self::NumberIsFinite => "isFinite",
            Self::NumberIsInteger => "isInteger",
            Self::NumberIsSafeInteger => "isSafeInteger",
            Self::NumberToExponential => "toExponential",
            Self::NumberToFixed => "toFixed",
            Self::NumberToPrecision => "toPrecision",
            Self::NumberToString => "toString",
            Self::NumberValueOf => "valueOf",
            Self::BooleanConstructor => "Boolean",
            Self::FunctionPrototype => "",
            Self::FunctionPrototypeCall => "call",
            Self::FunctionPrototypeBind => "bind",
            Self::FunctionConstructor => "Function",
            Self::ErrorConstructor(NativeErrorKind::Error) => "Error",
            Self::ErrorConstructor(NativeErrorKind::Reference) => "ReferenceError",
            Self::ErrorConstructor(NativeErrorKind::Syntax) => "SyntaxError",
            Self::ErrorConstructor(NativeErrorKind::Type) => "TypeError",
            Self::ErrorConstructor(NativeErrorKind::Range) => "RangeError",
            Self::ArrayConstructor => "Array",
            Self::ArrayIsArray => "isArray",
            Self::ArrayConcat => "concat",
            Self::ArrayPush => "push",
            Self::ArrayJoin => "join",
            Self::ArrayAt => "at",
            Self::ArrayIndexOf => "indexOf",
            Self::ArrayIncludes => "includes",
            Self::ArrayPop => "pop",
            Self::ArraySlice => "slice",
            Self::ArrayShift => "shift",
            Self::ArrayUnshift => "unshift",
            Self::ArrayReverse => "reverse",
            Self::ArrayFill => "fill",
            Self::ArrayLastIndexOf => "lastIndexOf",
            Self::ArrayCopyWithin => "copyWithin",
            Self::ArrayFlat => "flat",
            Self::ArraySort => "sort",
            Self::ArrayToString => "toString",
            Self::MathPow => "pow",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NativeErrorKind {
    Error,
    Reference,
    Syntax,
    Type,
    Range,
}

impl NativeErrorKind {
    const ALL: [Self; 5] = [
        Self::Error,
        Self::Reference,
        Self::Syntax,
        Self::Type,
        Self::Range,
    ];

    #[inline(always)]
    const fn index(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Reference => "ReferenceError",
            Self::Syntax => "SyntaxError",
            Self::Type => "TypeError",
            Self::Range => "RangeError",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ErrorIntrinsic {
    constructor: Option<Value>,
    prototype: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ErrorIntrinsics {
    entries: [ErrorIntrinsic; NativeErrorKind::ALL.len()],
}

impl ErrorIntrinsics {
    #[inline(always)]
    fn get(self, kind: NativeErrorKind) -> ErrorIntrinsic {
        self.entries[kind.index()]
    }

    #[inline(always)]
    fn get_mut(&mut self, kind: NativeErrorKind) -> &mut ErrorIntrinsic {
        &mut self.entries[kind.index()]
    }
}

impl Trace for ErrorIntrinsics {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for entry in &mut self.entries {
            entry.constructor.trace(tracer);
            entry.prototype.trace(tracer);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FunctionExecutable {
    Bytecode {
        code: CodeId,
        function: FunctionId,
        environment: Option<GcRef<Environment>>,
    },
    Native(NativeFunction),
    Bound(GcRef<BoundFunctionData>),
}

/// Callable payload with one explicit executable kind and shared ordinary-property storage.
#[derive(Clone, Copy, Debug)]
struct FunctionObject {
    executable: FunctionExecutable,
    function_prototype: Option<Value>,
    ordinary: OrdinaryObject,
}

#[derive(Clone, Copy, Debug)]
struct SymbolValue {
    description: Option<Value>,
}

impl Trace for SymbolValue {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.description.trace(tracer);
    }
}

#[derive(Clone, Copy)]
enum ObjectReceiver {
    Ordinary(GcRef<OrdinaryObject>),
    Array(GcRef<ArrayObject>),
    Function(GcRef<FunctionObject>),
    Number(GcRef<NumberObject>),
}

impl ObjectReceiver {
    #[inline(always)]
    fn value(self) -> Value {
        match self {
            Self::Ordinary(object) => Value::from_heap_ref(object.raw()),
            Self::Array(array) => Value::from_heap_ref(array.raw()),
            Self::Function(function) => Value::from_heap_ref(function.raw()),
            Self::Number(number) => Value::from_heap_ref(number.raw()),
        }
    }
}

#[derive(Clone, Copy)]
struct ResolvedCallTarget {
    code: CodeId,
    function: FunctionId,
    environment: Option<GcRef<Environment>>,
    layout: FunctionLayout,
    strictness: FunctionStrictness,
}

#[derive(Clone, Copy, Debug, Default)]
struct DataPropertyDescriptor {
    value: Option<Value>,
    writable: Option<bool>,
    enumerable: Option<bool>,
    configurable: Option<bool>,
}

#[derive(Clone, Copy)]
struct CallSite {
    caller_base: u32,
    destination: u32,
    callee: Value,
    argument_base: u32,
    argument_prefix: Option<GcRef<BoundFunctionData>>,
    argument_prefix_offset: u32,
    argument_prefix_count: u32,
    argument_count: u32,
    this_value: Value,
    new_target: Value,
    construct_receiver: Option<Value>,
    call_site: WordOffset,
}

#[derive(Clone, Copy)]
struct BoundFunctionSnapshot {
    bound_target: Value,
    call_target: Value,
    bound_this: Value,
    argument_count: u32,
    length: Value,
    name: Value,
}

impl Trace for FunctionObject {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        if let FunctionExecutable::Bytecode { environment, .. } = &mut self.executable {
            environment.trace(tracer);
        }
        if let FunctionExecutable::Bound(data) = &mut self.executable {
            data.trace(tracer);
        }
        self.function_prototype.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Clone, Copy)]
struct VmTypes {
    array: GcType<ArrayObject>,
    bound_function: GcType<BoundFunctionData>,
    environment: GcType<Environment>,
    for_in_iterator: GcType<ForInIterator>,
    function: GcType<FunctionObject>,
    number_object: GcType<NumberObject>,
    ordinary_object: GcType<OrdinaryObject>,
    property_storage: GcType<PropertyStorage>,
    string: GcType<JsString>,
    symbol: GcType<SymbolValue>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IntrinsicPropertyAtoms {
    prototype: Option<AtomId>,
    constructor: Option<AtomId>,
    message: Option<AtomId>,
    name: Option<AtomId>,
    length: Option<AtomId>,
}

#[derive(Clone, Copy, Debug)]
struct RealmIntrinsicAtoms {
    undefined: AtomId,
    nan: AtomId,
    infinity: AtomId,
    errors: [AtomId; NativeErrorKind::ALL.len()],
    array: AtomId,
    object: AtomId,
    string: AtomId,
    symbol: AtomId,
    number: AtomId,
    boolean: AtomId,
    function: AtomId,
    math: AtomId,
}

impl RealmIntrinsicAtoms {
    const BINDING_COUNT: usize = 11 + NativeErrorKind::ALL.len();

    #[inline(always)]
    fn error(self, kind: NativeErrorKind) -> AtomId {
        self.errors[kind.index()]
    }
}

/// An isolate-local immutable-code index; zero stays reserved for niche optimization and validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct CodeId(NonZeroU32);

impl CodeId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<CodeId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<CodeId>>()];

#[derive(Debug)]
struct LoadedCode {
    module: CompiledModule,
    scope_resolutions: Box<[ScopeResolution]>,
    constant_values: Box<[Option<Value>]>,
}

/// A batch-local view into verified immutable bytecode retained by the active `LoadedCode` module.
#[derive(Clone, Copy)]
struct BytecodeCursor {
    decoder: VerifiedInstructionDecoder<'static>,
    #[cfg(test)]
    bytecode: NonNull<VerifiedBytecode>,
}

impl BytecodeCursor {
    /// Captures one stable verified function without incrementing its backing reference counts.
    ///
    /// # Safety
    ///
    /// The owner of `bytecode` and its immutable word backing must outlive every use of the returned
    /// cursor. Moving the owner is allowed only when its verified functions remain in stable Arc
    /// storage; dropping or replacing that backing invalidates the cursor.
    unsafe fn new(bytecode: &VerifiedBytecode) -> Self {
        let decoder = VerifiedInstructionDecoder::new(bytecode);
        // SAFETY: This erases only the type-level borrow so mutable isolate slow paths can run. The
        // caller guarantees the backing owner outlives every use of the erased decoder.
        let decoder = unsafe {
            core::mem::transmute::<
                VerifiedInstructionDecoder<'_>,
                VerifiedInstructionDecoder<'static>,
            >(decoder)
        };
        Self {
            decoder,
            #[cfg(test)]
            bytecode: NonNull::from(bytecode),
        }
    }

    /// Decodes one verifier-proven instruction while the loaded module retains its immutable owner.
    ///
    /// # Safety
    ///
    /// `offset` must be an instruction start in the same verified bytecode passed to `new`, and that
    /// bytecode's owner must still be alive.
    #[inline(always)]
    unsafe fn decode(self, offset: WordOffset) -> DecodedInstruction {
        #[cfg(test)]
        {
            // SAFETY: `BytecodeCursor::new` requires the verified owner to outlive this cursor use.
            let bytecode = unsafe { self.bytecode.as_ref() };
            assert!(bytecode.is_instruction_start(offset));
        }
        // SAFETY: active frame PCs originate from verified fallthrough/jump/handler targets. Slow
        // exits publish one such PC before mutation; the caller carries that instruction-start proof.
        unsafe { self.decoder.decode_unchecked(offset) }
    }
}

/// Raw view of one verified activation's register window during a no-reallocation kernel epoch.
struct RegisterWindow {
    start: NonNull<Value>,
    len: usize,
}

impl RegisterWindow {
    /// Checks the activation boundary once before verified operands use unchecked slot access.
    fn new(registers: &mut [Value], base: usize, len: usize) -> Option<Self> {
        let end = base.checked_add(len)?;
        let window = registers.get_mut(base..end)?;
        Some(Self {
            start: NonNull::new(window.as_mut_ptr())
                .expect("slice pointers are non-null even for empty slices"),
            len,
        })
    }

    /// Reads an operand already proven in range by module verification and cursor entry.
    ///
    /// # Safety
    ///
    /// `register` must be below this window's verified length, and the owning register storage must
    /// not have been resized, reserved, truncated, or dropped since `RegisterWindow::new`.
    #[inline(always)]
    unsafe fn read(&self, register: u32) -> Value {
        let index = register as usize;
        debug_assert!(index < self.len);
        // SAFETY: The caller upholds the verified operand and no-reallocation epoch invariants.
        unsafe { *self.start.as_ptr().add(index) }
    }

    /// Writes an operand already proven in range without exposing a reference outside the cursor.
    ///
    /// # Safety
    ///
    /// `register` must be below this window's verified length, and this cursor must retain exclusive
    /// write access to the owning register storage for the complete no-reallocation epoch.
    #[inline(always)]
    unsafe fn write(&mut self, register: u32, value: Value) {
        let index = register as usize;
        debug_assert!(index < self.len);
        // SAFETY: The caller upholds the verified operand, exclusivity, and storage lifetime rules.
        unsafe { self.start.as_ptr().add(index).write(value) };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HotControl {
    Continue,
    Slow,
}

#[cfg(feature = "opcode-profile")]
#[inline(always)]
const fn is_conditional_branch(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish
    )
}

impl Trace for LoadedCode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for value in self.constant_values.iter_mut().flatten() {
            value.trace(tracer);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ScopeResolution {
    atom: AtomId,
    lexical_slot: Option<GlobalLexicalSlotId>,
    intrinsic_slot: Option<IntrinsicSlotId>,
    global_slot: Option<GlobalSlotId>,
}

#[derive(Clone, Copy, Debug)]
struct GlobalBinding {
    name: AtomId,
    value: Value,
}

#[derive(Clone, Copy, Debug)]
struct IntrinsicBinding {
    name: AtomId,
    value: Value,
    writable: bool,
}

/// Stable isolate-local index into mandatory bindings excluded from the host user-binding quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
struct IntrinsicSlotId(NonZeroU32);

impl IntrinsicSlotId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<IntrinsicSlotId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<IntrinsicSlotId>>()];

/// A stable isolate-local index into one realm's global binding storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
struct GlobalSlotId(NonZeroU32);

impl GlobalSlotId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<GlobalSlotId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<GlobalSlotId>>()];

#[derive(Clone, Copy, Debug)]
struct GlobalLexicalBinding {
    name: AtomId,
    value: Value,
    mutable: bool,
    initialized: bool,
}

/// A stable isolate-local index into the declarative global environment record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
struct GlobalLexicalSlotId(NonZeroU32);

impl GlobalLexicalSlotId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<GlobalLexicalSlotId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<GlobalLexicalSlotId>>()];

#[derive(Debug)]
struct Realm {
    intrinsic_bindings: Vec<IntrinsicBinding>,
    intrinsic_slots_by_atom: Vec<Option<IntrinsicSlotId>>,
    global_lexicals: Vec<GlobalLexicalBinding>,
    global_lexical_slots_by_atom: Vec<Option<GlobalLexicalSlotId>>,
    global_bindings: Vec<GlobalBinding>,
    global_slots_by_atom: Vec<Option<GlobalSlotId>>,
    global_object: Option<Value>,
    function_prototype: Option<Value>,
    function_prototype_call: Option<Value>,
    function_prototype_bind: Option<Value>,
    array_constructor: Option<Value>,
    array_prototype: Option<Value>,
    array_is_array: Option<Value>,
    array_concat: Option<Value>,
    array_push: Option<Value>,
    array_join: Option<Value>,
    array_at: Option<Value>,
    array_index_of: Option<Value>,
    array_includes: Option<Value>,
    array_pop: Option<Value>,
    array_slice: Option<Value>,
    array_shift: Option<Value>,
    array_unshift: Option<Value>,
    array_reverse: Option<Value>,
    array_fill: Option<Value>,
    array_last_index_of: Option<Value>,
    array_copy_within: Option<Value>,
    array_flat: Option<Value>,
    array_sort: Option<Value>,
    array_to_string: Option<Value>,
    object_constructor: Option<Value>,
    object_prototype: Option<Value>,
    object_define_property: Option<Value>,
    object_get_own_property_descriptor: Option<Value>,
    object_get_own_property_names: Option<Value>,
    object_has_own_property: Option<Value>,
    object_property_is_enumerable: Option<Value>,
    object_to_string: Option<Value>,
    object_assign: Option<Value>,
    object_keys: Option<Value>,
    object_values: Option<Value>,
    object_entries: Option<Value>,
    object_has_own: Option<Value>,
    object_is: Option<Value>,
    object_get_prototype_of: Option<Value>,
    object_create: Option<Value>,
    object_is_prototype_of: Option<Value>,
    object_is_extensible: Option<Value>,
    object_prevent_extensions: Option<Value>,
    string_constructor: Option<Value>,
    symbol_constructor: Option<Value>,
    number_constructor: Option<Value>,
    number_prototype: Option<Value>,
    number_is_nan: Option<Value>,
    number_is_finite: Option<Value>,
    number_is_integer: Option<Value>,
    number_is_safe_integer: Option<Value>,
    number_to_exponential: Option<Value>,
    number_to_fixed: Option<Value>,
    number_to_precision: Option<Value>,
    number_to_string: Option<Value>,
    number_value_of: Option<Value>,
    boolean_constructor: Option<Value>,
    function_constructor: Option<Value>,
    math_object: Option<Value>,
    math_pow: Option<Value>,
    error_intrinsics: ErrorIntrinsics,
    typeof_strings: TypeofStrings,
    limits: RealmLimits,
}

impl Realm {
    fn new(limits: RealmLimits, typeof_strings: TypeofStrings) -> Self {
        Self {
            intrinsic_bindings: Vec::new(),
            intrinsic_slots_by_atom: Vec::new(),
            global_lexicals: Vec::new(),
            global_lexical_slots_by_atom: Vec::new(),
            global_bindings: Vec::new(),
            global_slots_by_atom: Vec::new(),
            global_object: None,
            function_prototype: None,
            function_prototype_call: None,
            function_prototype_bind: None,
            array_constructor: None,
            array_prototype: None,
            array_is_array: None,
            array_concat: None,
            array_push: None,
            array_join: None,
            array_at: None,
            array_index_of: None,
            array_includes: None,
            array_pop: None,
            array_slice: None,
            array_shift: None,
            array_unshift: None,
            array_reverse: None,
            array_fill: None,
            array_last_index_of: None,
            array_copy_within: None,
            array_flat: None,
            array_sort: None,
            array_to_string: None,
            object_constructor: None,
            object_prototype: None,
            object_define_property: None,
            object_get_own_property_descriptor: None,
            object_get_own_property_names: None,
            object_has_own_property: None,
            object_property_is_enumerable: None,
            object_to_string: None,
            object_assign: None,
            object_keys: None,
            object_values: None,
            object_entries: None,
            object_has_own: None,
            object_is: None,
            object_get_prototype_of: None,
            object_create: None,
            object_is_prototype_of: None,
            object_is_extensible: None,
            object_prevent_extensions: None,
            string_constructor: None,
            symbol_constructor: None,
            number_constructor: None,
            number_prototype: None,
            number_is_nan: None,
            number_is_finite: None,
            number_is_integer: None,
            number_is_safe_integer: None,
            number_to_exponential: None,
            number_to_fixed: None,
            number_to_precision: None,
            number_to_string: None,
            number_value_of: None,
            boolean_constructor: None,
            function_constructor: None,
            math_object: None,
            math_pow: None,
            error_intrinsics: ErrorIntrinsics::default(),
            typeof_strings,
            limits,
        }
    }

    /// Reserves the complete mandatory binding set before any intrinsic becomes observable.
    fn reserve_intrinsics(
        &mut self,
        binding_count: usize,
        atom_upper_bound: usize,
    ) -> Result<(), ExecutionError> {
        self.intrinsic_bindings
            .try_reserve_exact(binding_count)
            .map_err(|_| ExecutionError::IntrinsicBindingAllocationFailed)?;
        self.intrinsic_slots_by_atom
            .try_reserve_exact(atom_upper_bound)
            .map_err(|_| ExecutionError::IntrinsicBindingIndexAllocationFailed)?;
        self.intrinsic_slots_by_atom.resize(atom_upper_bound, None);
        Ok(())
    }

    /// Publishes one pre-reserved intrinsic with stable identity and explicit writability.
    fn publish_intrinsic(
        &mut self,
        name: AtomId,
        value: Value,
        writable: bool,
    ) -> Result<(), ExecutionError> {
        let slot = IntrinsicSlotId::from_index(self.intrinsic_bindings.len())
            .ok_or(ExecutionError::IntrinsicBindingAllocationFailed)?;
        let target = self
            .intrinsic_slots_by_atom
            .get_mut(name.index() as usize)
            .ok_or(ExecutionError::IntrinsicBindingIndexAllocationFailed)?;
        debug_assert!(target.is_none());
        self.intrinsic_bindings.push(IntrinsicBinding {
            name,
            value,
            writable,
        });
        *target = Some(slot);
        Ok(())
    }

    #[inline(always)]
    fn resolve_intrinsic(&self, name: AtomId) -> Option<IntrinsicSlotId> {
        let slot = self
            .intrinsic_slots_by_atom
            .get(name.index() as usize)
            .copied()
            .flatten()?;
        debug_assert_eq!(self.intrinsic_bindings[slot.index()].name, name);
        Some(slot)
    }

    #[inline(always)]
    fn intrinsic_value(&self, slot: IntrinsicSlotId) -> Value {
        self.intrinsic_bindings[slot.index()].value
    }

    #[inline(always)]
    fn set_intrinsic(&mut self, slot: IntrinsicSlotId, value: Value) -> Result<(), ExecutionError> {
        let binding = &mut self.intrinsic_bindings[slot.index()];
        if !binding.writable {
            return Err(ExecutionError::ReadOnlyBinding(binding.name));
        }
        binding.value = value;
        Ok(())
    }

    #[inline(always)]
    fn resolve_lexical(&self, name: AtomId) -> Option<GlobalLexicalSlotId> {
        let slot = self
            .global_lexical_slots_by_atom
            .get(name.index() as usize)
            .copied()
            .flatten()?;
        debug_assert_eq!(self.global_lexicals[slot.index()].name, name);
        Some(slot)
    }

    fn lexical_value(&self, slot: GlobalLexicalSlotId) -> Result<Value, ExecutionError> {
        let binding = &self.global_lexicals[slot.index()];
        if binding.initialized {
            Ok(binding.value)
        } else {
            Err(ExecutionError::UninitializedBinding(binding.name))
        }
    }

    /// Publishes an uninitialized declarative binding after reserving both stable-index tables.
    fn declare_lexical(&mut self, name: AtomId, mutable: bool) -> Result<(), ExecutionError> {
        if self.resolve_lexical(name).is_some()
            || self.resolve_intrinsic(name).is_some()
            || self.resolve(name).is_some()
        {
            return Err(ExecutionError::GlobalLexicalRedeclaration(name));
        }
        if self
            .global_lexicals
            .len()
            .saturating_add(self.global_bindings.len())
            >= self.limits.max_global_bindings as usize
        {
            return Err(ExecutionError::GlobalBindingLimit {
                limit: self.limits.max_global_bindings,
            });
        }
        let required_slots = (name.index() as usize)
            .checked_add(1)
            .ok_or(ExecutionError::GlobalBindingIndexAllocationFailed)?;
        let additional_slots =
            required_slots.saturating_sub(self.global_lexical_slots_by_atom.len());
        self.global_lexical_slots_by_atom
            .try_reserve_exact(additional_slots)
            .map_err(|_| ExecutionError::GlobalBindingIndexAllocationFailed)?;
        self.global_lexicals
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::GlobalBindingAllocationFailed)?;
        self.global_lexical_slots_by_atom
            .resize(required_slots, None);
        let slot = GlobalLexicalSlotId::from_index(self.global_lexicals.len())
            .ok_or(ExecutionError::GlobalBindingLimit { limit: u32::MAX })?;
        self.global_lexicals.push(GlobalLexicalBinding {
            name,
            value: Value::from_immediate(Immediate::Undefined),
            mutable,
            initialized: false,
        });
        self.global_lexical_slots_by_atom[name.index() as usize] = Some(slot);
        Ok(())
    }

    fn initialize_lexical(
        &mut self,
        slot: GlobalLexicalSlotId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let binding = &mut self.global_lexicals[slot.index()];
        if binding.initialized {
            return Err(ExecutionError::GlobalLexicalAlreadyInitialized(
                binding.name,
            ));
        }
        binding.value = value;
        binding.initialized = true;
        Ok(())
    }

    fn set_lexical(
        &mut self,
        slot: GlobalLexicalSlotId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let binding = &mut self.global_lexicals[slot.index()];
        if !binding.initialized {
            return Err(ExecutionError::UninitializedBinding(binding.name));
        }
        if !binding.mutable {
            return Err(ExecutionError::ImmutableBinding(binding.name));
        }
        binding.value = value;
        Ok(())
    }

    #[inline(always)]
    fn get_slot(&self, slot: GlobalSlotId) -> Option<Value> {
        self.global_bindings
            .get(slot.index())
            .map(|binding| binding.value)
    }

    #[inline(always)]
    fn set_slot(&mut self, slot: GlobalSlotId, value: Value) {
        self.global_bindings[slot.index()].value = value;
    }

    /// Updates an existing slot or atomically publishes one after both backing reserves succeed.
    fn set(&mut self, name: AtomId, value: Value) -> Result<(), ExecutionError> {
        if self.resolve_lexical(name).is_some() {
            return Err(ExecutionError::GlobalLexicalRedeclaration(name));
        }
        if let Some(slot) = self.resolve_intrinsic(name) {
            return self.set_intrinsic(slot, value);
        }
        if let Some(slot) = self.resolve(name) {
            self.set_slot(slot, value);
            return Ok(());
        }
        if self
            .global_lexicals
            .len()
            .saturating_add(self.global_bindings.len())
            >= self.limits.max_global_bindings as usize
        {
            return Err(ExecutionError::GlobalBindingLimit {
                limit: self.limits.max_global_bindings,
            });
        }
        let required_slots = (name.index() as usize)
            .checked_add(1)
            .ok_or(ExecutionError::GlobalBindingIndexAllocationFailed)?;
        let additional_slots = required_slots.saturating_sub(self.global_slots_by_atom.len());
        self.global_slots_by_atom
            .try_reserve_exact(additional_slots)
            .map_err(|_| ExecutionError::GlobalBindingIndexAllocationFailed)?;
        self.global_bindings
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::GlobalBindingAllocationFailed)?;
        self.global_slots_by_atom.resize(required_slots, None);
        let slot = GlobalSlotId::from_index(self.global_bindings.len())
            .ok_or(ExecutionError::GlobalBindingLimit { limit: u32::MAX })?;
        self.global_bindings.push(GlobalBinding { name, value });
        self.global_slots_by_atom[name.index() as usize] = Some(slot);
        Ok(())
    }

    #[inline(always)]
    fn resolve(&self, name: AtomId) -> Option<GlobalSlotId> {
        let slot = self
            .global_slots_by_atom
            .get(name.index() as usize)
            .copied()
            .flatten()?;
        debug_assert_eq!(self.global_bindings[slot.index()].name, name);
        Some(slot)
    }
}

impl Trace for Realm {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for binding in &mut self.intrinsic_bindings {
            binding.value.trace(tracer);
        }
        for binding in &mut self.global_lexicals {
            binding.value.trace(tracer);
        }
        for binding in &mut self.global_bindings {
            binding.value.trace(tracer);
        }
        self.global_object.trace(tracer);
        self.function_prototype.trace(tracer);
        self.function_prototype_call.trace(tracer);
        self.function_prototype_bind.trace(tracer);
        self.array_constructor.trace(tracer);
        self.array_prototype.trace(tracer);
        self.array_is_array.trace(tracer);
        self.array_concat.trace(tracer);
        self.array_push.trace(tracer);
        self.array_join.trace(tracer);
        self.array_at.trace(tracer);
        self.array_index_of.trace(tracer);
        self.array_includes.trace(tracer);
        self.array_pop.trace(tracer);
        self.array_slice.trace(tracer);
        self.array_shift.trace(tracer);
        self.array_unshift.trace(tracer);
        self.array_reverse.trace(tracer);
        self.array_fill.trace(tracer);
        self.array_last_index_of.trace(tracer);
        self.array_copy_within.trace(tracer);
        self.array_flat.trace(tracer);
        self.array_sort.trace(tracer);
        self.array_to_string.trace(tracer);
        self.object_constructor.trace(tracer);
        self.object_prototype.trace(tracer);
        self.object_define_property.trace(tracer);
        self.object_get_own_property_descriptor.trace(tracer);
        self.object_get_own_property_names.trace(tracer);
        self.object_has_own_property.trace(tracer);
        self.object_property_is_enumerable.trace(tracer);
        self.object_to_string.trace(tracer);
        self.object_assign.trace(tracer);
        self.object_keys.trace(tracer);
        self.object_values.trace(tracer);
        self.object_entries.trace(tracer);
        self.object_has_own.trace(tracer);
        self.object_is.trace(tracer);
        self.object_get_prototype_of.trace(tracer);
        self.object_create.trace(tracer);
        self.object_is_prototype_of.trace(tracer);
        self.object_is_extensible.trace(tracer);
        self.object_prevent_extensions.trace(tracer);
        self.string_constructor.trace(tracer);
        self.symbol_constructor.trace(tracer);
        self.number_constructor.trace(tracer);
        self.number_prototype.trace(tracer);
        self.number_is_nan.trace(tracer);
        self.number_is_finite.trace(tracer);
        self.number_is_integer.trace(tracer);
        self.number_is_safe_integer.trace(tracer);
        self.number_to_exponential.trace(tracer);
        self.number_to_fixed.trace(tracer);
        self.number_to_precision.trace(tracer);
        self.number_to_string.trace(tracer);
        self.number_value_of.trace(tracer);
        self.boolean_constructor.trace(tracer);
        self.function_constructor.trace(tracer);
        self.math_object.trace(tracer);
        self.math_pow.trace(tracer);
        self.error_intrinsics.trace(tracer);
        self.typeof_strings.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug)]
struct TypeofStrings {
    undefined: Value,
    object: Value,
    boolean: Value,
    number: Value,
    string: Value,
    function: Value,
    symbol: Value,
    bigint: Value,
}

impl Trace for TypeofStrings {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.undefined.trace(tracer);
        self.object.trace(tracer);
        self.boolean.trace(tracer);
        self.number.trace(tracer);
        self.string.trace(tracer);
        self.function.trace(tracer);
        self.symbol.trace(tracer);
        self.bigint.trace(tracer);
    }
}

impl TypeofStrings {
    /// Allocates the complete spec-fixed typeof vocabulary once before the isolate becomes visible.
    fn allocate(
        heap: &mut Heap,
        string_type: GcType<JsString>,
    ) -> Result<Self, IsolateCreationError> {
        Ok(Self {
            undefined: allocate_initial_string(heap, string_type, b"undefined")?,
            object: allocate_initial_string(heap, string_type, b"object")?,
            boolean: allocate_initial_string(heap, string_type, b"boolean")?,
            number: allocate_initial_string(heap, string_type, b"number")?,
            string: allocate_initial_string(heap, string_type, b"string")?,
            function: allocate_initial_string(heap, string_type, b"function")?,
            symbol: allocate_initial_string(heap, string_type, b"symbol")?,
            bigint: allocate_initial_string(heap, string_type, b"bigint")?,
        })
    }
}

fn allocate_initial_string(
    heap: &mut Heap,
    string_type: GcType<JsString>,
    bytes: &[u8],
) -> Result<Value, IsolateCreationError> {
    let string = JsString::try_from_latin1(bytes).map_err(IsolateCreationError::String)?;
    let reference = heap
        .try_allocate_external(string_type, 0, string, AllocationSpace::Old)
        .map_err(IsolateCreationError::HeapAllocation)?;
    Ok(Value::from_heap_ref(reference.raw()))
}

struct VmRoots<'a> {
    fiber: &'a mut Fiber,
    finalization_jobs: &'a mut finalization::FinalizationJobs,
    realm: &'a mut Realm,
    loaded_code: &'a mut Vec<LoadedCode>,
}

struct PropertyMutationRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
}

struct PrototypeInitializationRoots<'a> {
    vm: VmRoots<'a>,
    function: Value,
}

struct ArrayAllocationRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
}

impl Trace for PropertyMutationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
    }
}

impl Trace for PrototypeInitializationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.function.trace(tracer);
    }
}

impl Trace for ArrayAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Trace for VmRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        self.finalization_jobs.trace(tracer);
        self.realm.trace(tracer);
        for code in self.loaded_code.iter_mut() {
            code.trace(tracer);
        }
    }
}

struct CodeLoadRoots<'a> {
    vm: VmRoots<'a>,
    constant_values: &'a mut Vec<Option<Value>>,
}

#[derive(Clone, Copy, Debug)]
struct NativeContinuationSite {
    caller_base: u32,
    destination: u32,
    call_site: WordOffset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToPrimitiveStage {
    ValueOf,
    ToString,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConversionConsumer {
    NativeCall(NativeFunction),
    NativeConstruct(NativeFunction),
    ToNumber,
    Negate,
    BitwiseNot,
    BinaryLeft(Opcode),
    BinaryRight(Opcode),
    AddLeft,
    AddRight,
    RelationalLeft(Opcode),
    RelationalRight(Opcode),
}

impl ConversionConsumer {
    #[inline]
    const fn native(self) -> Option<NativeFunction> {
        match self {
            Self::NativeCall(native) | Self::NativeConstruct(native) => Some(native),
            Self::ToNumber
            | Self::Negate
            | Self::BitwiseNot
            | Self::BinaryLeft(_)
            | Self::BinaryRight(_)
            | Self::AddLeft
            | Self::AddRight
            | Self::RelationalLeft(_)
            | Self::RelationalRight(_) => None,
        }
    }

    #[inline]
    const fn uses_string_hint(self) -> bool {
        matches!(self, Self::NativeCall(NativeFunction::StringConstructor))
    }

    #[inline]
    const fn is_opcode_conversion(self) -> bool {
        matches!(
            self,
            Self::ToNumber
                | Self::Negate
                | Self::BitwiseNot
                | Self::BinaryLeft(_)
                | Self::BinaryRight(_)
                | Self::AddLeft
                | Self::AddRight
                | Self::RelationalLeft(_)
                | Self::RelationalRight(_)
        )
    }
}

#[inline]
fn next_to_primitive_stage(
    consumer: ConversionConsumer,
    stage: ToPrimitiveStage,
) -> Option<ToPrimitiveStage> {
    if consumer.uses_string_hint() {
        match stage {
            ToPrimitiveStage::ToString => Some(ToPrimitiveStage::ValueOf),
            ToPrimitiveStage::ValueOf => None,
        }
    } else {
        match stage {
            ToPrimitiveStage::ValueOf => Some(ToPrimitiveStage::ToString),
            ToPrimitiveStage::ToString => None,
        }
    }
}

/// Resumable native work owned by a JS callback frame instead of the Rust call stack.
#[derive(Clone, Copy, Debug)]
struct NativeContinuation {
    site: NativeContinuationSite,
    consumer: ConversionConsumer,
    receiver: Value,
    object: Value,
    stage: ToPrimitiveStage,
}

impl Trace for NativeContinuation {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.object.trace(tracer);
    }
}

impl Trace for CodeLoadRoots<'_> {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        for value in self.constant_values.iter_mut().flatten() {
            value.trace(tracer);
        }
    }
}

/// One explicit JavaScript activation. Rust stack frames never represent JavaScript calls.
#[derive(Clone, Copy, Debug)]
struct Frame {
    code: CodeId,
    function: FunctionId,
    pc: WordOffset,
    base: u32,
    environment: Option<GcRef<Environment>>,
    return_register: Option<RegisterId>,
    return_continuation: bool,
    this_value: Value,
    new_target: Value,
    construct_receiver: Option<Value>,
    strictness: FunctionStrictness,
    argument_base: u32,
    argument_prefix: Option<GcRef<BoundFunctionData>>,
    argument_prefix_offset: u32,
    argument_prefix_count: u32,
    argument_count: u32,
    handler_base: u32,
    completion_base: u32,
    call_site: Option<WordOffset>,
}

/// The dynamic handler state selected from immutable bytecode handler metadata.
#[derive(Clone, Copy, Debug)]
struct ActiveHandler {
    handler_index: u32,
    frame_depth: u32,
    environment_depth: u32,
}

/// Abrupt completions are data, so throw/finally never need Rust stack unwinding.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Populated by Throw/finally lowering after handler dispatch is implemented.
enum Completion {
    Return(Value),
    Throw(Value),
    Native(NativeContinuation),
}

#[derive(Debug, Default)]
struct Fiber {
    frames: Vec<Frame>,
    registers: Vec<Value>,
    handlers: Vec<ActiveHandler>,
    completions: Vec<Completion>,
    pending_exception: Option<Value>,
}

impl Fiber {
    /// Traces every mutable reference reachable from an active, yielded, or suspended fiber.
    ///
    /// Frame control indices are validated when handlers are installed. They do not themselves
    /// own heap references, while registers, frame context, and abrupt completion payloads do.
    fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.registers.trace(tracer);
        for frame in &mut self.frames {
            frame.environment.trace(tracer);
            frame.this_value.trace(tracer);
            frame.new_target.trace(tracer);
            frame.construct_receiver.trace(tracer);
            frame.argument_prefix.trace(tracer);
            if let Some(return_register) = frame.return_register {
                debug_assert!((return_register.index() as usize) < self.registers.len());
            }
            debug_assert!(frame.argument_prefix_count <= frame.argument_count);
            debug_assert!(
                frame.argument_prefix.is_some()
                    || (frame.argument_prefix_offset == 0 && frame.argument_prefix_count == 0)
            );
            debug_assert!(
                frame
                    .argument_base
                    .checked_add(
                        frame
                            .argument_count
                            .saturating_sub(frame.argument_prefix_count),
                    )
                    .is_some_and(|end| end as usize <= self.registers.len())
            );
            let _is_strict = matches!(frame.strictness, FunctionStrictness::Strict);
        }
        for handler in &self.handlers {
            debug_assert!(
                usize::try_from(handler.frame_depth).is_ok_and(|depth| depth <= self.frames.len())
            );
            debug_assert!(
                usize::try_from(handler.environment_depth)
                    .is_ok_and(|depth| depth <= self.frames.len())
            );
            let _ = handler.handler_index;
        }
        for completion in &mut self.completions {
            match completion {
                Completion::Return(value) | Completion::Throw(value) => value.trace(tracer),
                Completion::Native(continuation) => continuation.trace(tracer),
            }
        }
        self.pending_exception.trace(tracer);
    }
}

/// A single-thread-owned ECMAScript execution state; `Cell` intentionally makes it `!Sync`.
pub struct Isolate {
    fiber: Fiber,
    finalization_jobs: finalization::FinalizationJobs,
    atoms: AtomTable,
    shapes: ShapeTable,
    realm: Realm,
    loaded_code: Vec<LoadedCode>,
    heap: Heap,
    types: VmTypes,
    intrinsic_property_atoms: IntrinsicPropertyAtoms,
    stack_limits: StackLimits,
    #[cfg(feature = "opcode-profile")]
    execution_profile: ExecutionProfile,
    _not_sync: Cell<()>,
}

impl Isolate {
    /// Registers VM payload descriptors before constructing an otherwise empty isolate heap.
    pub fn new(config: IsolateConfig) -> Result<Self, IsolateCreationError> {
        let mut registry = TypeRegistry::new();
        let types = VmTypes {
            array: registry
                .try_register("ArrayObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            bound_function: registry
                .try_register("BoundFunctionData")
                .map_err(IsolateCreationError::TypeRegistration)?,
            environment: registry
                .try_register("Environment")
                .map_err(IsolateCreationError::TypeRegistration)?,
            for_in_iterator: registry
                .try_register("ForInIterator")
                .map_err(IsolateCreationError::TypeRegistration)?,
            function: registry
                .try_register("FunctionObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            number_object: registry
                .try_register("NumberObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            ordinary_object: registry
                .try_register("OrdinaryObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            property_storage: registry
                .try_register("PropertyStorage")
                .map_err(IsolateCreationError::TypeRegistration)?,
            string: registry
                .try_register("JsString")
                .map_err(IsolateCreationError::TypeRegistration)?,
            symbol: registry
                .try_register("SymbolValue")
                .map_err(IsolateCreationError::TypeRegistration)?,
        };
        let shapes =
            ShapeTable::new(config.realm_limits.max_shapes).map_err(IsolateCreationError::Shape)?;
        let mut heap = Heap::new(config.heap_limit, registry);
        let typeof_strings = TypeofStrings::allocate(&mut heap, types.string)?;
        let mut isolate = Self {
            fiber: Fiber::default(),
            finalization_jobs: finalization::FinalizationJobs::new(),
            atoms: AtomTable::new(config.atom_table),
            shapes,
            realm: Realm::new(config.realm_limits, typeof_strings),
            loaded_code: Vec::new(),
            heap,
            types,
            intrinsic_property_atoms: IntrinsicPropertyAtoms::default(),
            stack_limits: config.stack_limits,
            #[cfg(feature = "opcode-profile")]
            execution_profile: ExecutionProfile::default(),
            _not_sync: Cell::new(()),
        };
        isolate
            .initialize_realm_intrinsics()
            .map_err(IsolateCreationError::IntrinsicInitialization)?;
        Ok(isolate)
    }

    #[must_use]
    pub const fn atoms(&self) -> &AtomTable {
        &self.atoms
    }

    pub const fn atoms_mut(&mut self) -> &mut AtomTable {
        &mut self.atoms
    }

    /// Returns the opt-in interpreter profile accumulated by this isolate.
    #[cfg(feature = "opcode-profile")]
    #[must_use]
    pub const fn execution_profile(&self) -> &ExecutionProfile {
        &self.execution_profile
    }

    /// Clears every opt-in interpreter counter without changing executable state.
    #[cfg(feature = "opcode-profile")]
    pub fn reset_execution_profile(&mut self) {
        self.execution_profile = ExecutionProfile::default();
    }

    /// Classifies a managed error through its intrinsic prototype chain without exposing heap IDs.
    pub fn native_error_kind(
        &mut self,
        value: Value,
    ) -> Result<Option<NativeErrorKind>, ExecutionError> {
        let mut current = value;
        loop {
            for kind in NativeErrorKind::ALL {
                if self.realm.error_intrinsics.get(kind).prototype == Some(current) {
                    return Ok(Some(kind));
                }
            }
            if !self.is_object_value(current) {
                return Ok(None);
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(None);
            }
            current = snapshot.prototype;
        }
    }

    fn allocate_intrinsic_ordinary_object(
        &mut self,
        ordinary: OrdinaryObject,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                ordinary,
                AllocationSpace::Old,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    fn intern_intrinsic_name(&mut self, name: &[u8]) -> Result<AtomId, ExecutionError> {
        let string = JsString::try_from_latin1(name).map_err(ExecutionError::PropertyKeyString)?;
        self.atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)
    }

    /// Allocates one native callable through the same managed function descriptor as bytecode code.
    fn allocate_native_function(
        &mut self,
        native: NativeFunction,
        ordinary: OrdinaryObject,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Native(native),
                    function_prototype: None,
                    ordinary,
                },
                AllocationSpace::Old,
                roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Creates one bound exotic while flattening nested wrappers into one immutable argument prefix.
    fn create_bound_function(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let bound_target = site.this_value;
        self.resolve_function_object(bound_target)?;
        let length =
            self.bound_function_length(bound_target, site.argument_count.saturating_sub(1))?;
        let name = self.allocate_bound_function_name(bound_target)?;
        self.write(site.caller_base, site.destination, name)?;
        let supplied_this = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let target_object = self.resolve_function_object(bound_target)?;
        let (call_target, bound_this, existing_arguments) = match target_object.executable {
            FunctionExecutable::Bound(data) => {
                let snapshot = self.bound_function_snapshot(data)?;
                (snapshot.call_target, snapshot.bound_this, Some(data))
            }
            _ => (bound_target, supplied_this, None),
        };
        let existing_count = existing_arguments
            .map(|data| {
                self.bound_function_snapshot(data)
                    .map(|data| data.argument_count)
            })
            .transpose()?
            .unwrap_or(0);
        let supplied_count = site.argument_count.saturating_sub(1);
        let argument_count = existing_count
            .checked_add(supplied_count)
            .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(argument_count as usize)
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        if let Some(data) = existing_arguments {
            self.append_bound_arguments(data, &mut arguments)?;
        }
        for index in 0..supplied_count {
            arguments.push(
                self.call_argument(site, index + 1)?
                    .expect("supplied bound argument is within the call window"),
            );
        }
        let data = {
            let roots = &mut VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            };
            self.heap
                .try_allocate_external_with_gc(
                    self.types.bound_function,
                    0,
                    BoundFunctionData {
                        bound_target,
                        call_target,
                        bound_this,
                        arguments: arguments.into_boxed_slice(),
                        length,
                        name,
                    },
                    AllocationSpace::Young,
                    roots,
                )
                .map_err(ExecutionError::HeapAllocation)?
        };
        let internal_prototype = self
            .resolve_function_object(site.this_value)?
            .ordinary
            .prototype;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let function = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Bound(data),
                    function_prototype: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: internal_prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(function.raw()))
    }

    /// Computes the configurable bound length from the target's own numeric length property.
    fn bound_function_length(
        &mut self,
        target: Value,
        supplied_arguments: u32,
    ) -> Result<Value, ExecutionError> {
        let length_atom = self.length_atom()?;
        let Some((length, _)) = self.own_data_property_with_attributes(target, length_atom)? else {
            return Ok(Value::from_i32(0));
        };
        let Some(length) = numeric_value(length) else {
            return Ok(Value::from_i32(0));
        };
        if length == f64::INFINITY {
            return Ok(Value::from_f64(f64::INFINITY));
        }
        let length = length.trunc().max(0.0) - f64::from(supplied_arguments);
        Ok(Value::from_f64(length.max(0.0)))
    }

    /// Materializes `"bound " + targetName` with one exact UTF-16 reserve before GC allocation.
    fn allocate_bound_function_name(&mut self, target: Value) -> Result<Value, ExecutionError> {
        const PREFIX: &[u8] = b"bound ";
        let name_atom = self.name_atom()?;
        let target_name = self
            .get_data_property(target, name_atom)?
            .filter(|value| self.is_string_value(*value));
        let target_length = target_name
            .map(|value| self.string_value_length(value))
            .transpose()?
            .unwrap_or(0);
        let capacity = PREFIX
            .len()
            .checked_add(target_length)
            .ok_or(ExecutionError::BoundNameAllocationFailed)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::BoundNameAllocationFailed)?;
        units.extend(PREFIX.iter().map(|&byte| u16::from(byte)));
        if let Some(target_name) = target_name {
            self.append_primitive_string_units(target_name, &mut units)?;
        }
        let name = JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(name)
    }

    #[inline(always)]
    fn is_string_value(&self, value: Value) -> bool {
        value
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.string).is_ok())
    }

    #[inline(always)]
    fn is_symbol_value(&self, value: Value) -> bool {
        value
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.symbol).is_ok())
    }

    fn string_value_length(&mut self, value: Value) -> Result<usize, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(value))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedStringValue(value))?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(JsString::len)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn bound_function_snapshot(
        &mut self,
        data: GcRef<BoundFunctionData>,
    ) -> Result<BoundFunctionSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.bound_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(BoundFunctionSnapshot {
                    bound_target: data.bound_target,
                    call_target: data.call_target,
                    bound_this: data.bound_this,
                    argument_count: u32::try_from(data.arguments.len())
                        .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
                    length: data.length,
                    name: data.name,
                })
            })
        })
    }

    fn bound_function_argument(
        &mut self,
        data: GcRef<BoundFunctionData>,
        index: u32,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.bound_function)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .arguments
                    .get(index as usize)
                    .copied()
                    .ok_or(ExecutionError::BoundArgumentCountOverflow)
            })
        })
    }

    fn append_bound_arguments(
        &mut self,
        data: GcRef<BoundFunctionData>,
        output: &mut Vec<Value>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.bound_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                output.extend_from_slice(&data.arguments);
                Ok(())
            })
        })
    }

    /// Allocates an empty ordinary object with a caller-selected prototype through managed GC.
    fn create_ordinary_object(&mut self) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before ordinary objects");
        self.create_ordinary_object_with_prototype(prototype)
    }

    /// Keeps a prototype edge in the pending payload so pre-allocation collection can rewrite it.
    fn create_ordinary_object_with_prototype(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let object = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(object.raw()))
    }

    /// Allocates one boxed Number while keeping its data and prototype live across collection.
    fn allocate_number_object(
        &mut self,
        number_data: Value,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        debug_assert!(numeric_value(number_data).is_some());
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.number_object,
                0,
                0,
                NumberObject {
                    number_data,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                space,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one Array exotic while keeping its ordinary prototype edge in the pending payload.
    fn create_array_object_with_prototype(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        self.allocate_array_object(prototype, AllocationSpace::Young)
    }

    /// Publishes the mandatory length slot before exposing one Array exotic identity.
    fn allocate_array_object(
        &mut self,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        let length = self.length_atom()?;
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                length,
                PropertyAttributes::data(true, false, false),
            )
            .map_err(ExecutionError::Shape)?;
        let mut roots = ArrayAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage {
                    slots: Box::new([Value::from_i32(0)]),
                },
                space,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let array = self
            .heap
            .try_allocate_with_gc(
                self.types.array,
                0,
                0,
                ArrayObject {
                    ordinary: OrdinaryObject {
                        shape,
                        extensible: true,
                        storage: Some(storage),
                        prototype: roots.prototype,
                    },
                },
                space,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(array.raw()))
    }

    /// Converts the non-numeric primitive PropertyKeys handled before the shared numeric path.
    fn property_key_atom_or_undefined(&mut self, value: Value) -> Result<AtomId, ExecutionError> {
        match value.as_immediate() {
            Some(Immediate::Undefined) => self.intern_intrinsic_name(b"undefined"),
            Some(Immediate::Null) => self.intern_intrinsic_name(b"null"),
            Some(Immediate::True) => self.intern_intrinsic_name(b"true"),
            Some(Immediate::False) => self.intern_intrinsic_name(b"false"),
            _ => self.property_key_atom(value),
        }
    }

    #[inline(always)]
    fn safe_integer_property_atom(&mut self, index: u64) -> Result<AtomId, ExecutionError> {
        debug_assert!(index <= MAX_SAFE_INTEGER);
        if let Ok(integer) = i32::try_from(index) {
            return self.property_key_atom(Value::from_i32(integer));
        }
        self.property_key_atom(Value::from_f64(index as f64))
    }

    /// Walks ordinary prototype links without allocating or invoking accessor/exotic behavior.
    fn get_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        let mut current = if numeric_value(receiver).is_some() {
            self.realm
                .number_prototype
                .expect("Number prototype initializes before property access")
        } else {
            receiver
        };
        loop {
            let (_, snapshot) = self.object_snapshot(current)?;
            if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
                if let Some(value) = self.property_value_from_snapshot(snapshot, property)? {
                    return Ok(Some(value));
                }
            } else {
                if let Some(value) = self.function_metadata_property(current, key)? {
                    return Ok(Some(value));
                }
                if self.is_function_prototype_property(current, key) {
                    self.intrinsic_property_atoms.prototype = Some(key);
                    return self.ensure_function_prototype(current).map(Some);
                }
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(None);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Reads only an object's own data slot, excluding inherited prototype properties.
    fn has_own_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<bool, ExecutionError> {
        Ok(self
            .own_data_property_with_attributes(receiver, key)?
            .is_some())
    }

    /// Resolves virtual function fields and ordinary own slots with their exact data flags.
    fn own_data_property_with_attributes(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<Option<(Value, PropertyAttributes)>, ExecutionError> {
        let (_, snapshot) = self.object_snapshot(receiver)?;
        if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
            return Ok(self
                .property_value_from_snapshot(snapshot, property)?
                .map(|value| (value, property.attributes)));
        }
        if let Some(value) = self.function_metadata_property(receiver, key)? {
            return Ok(Some((value, PropertyAttributes::data(false, false, true))));
        }
        if self.is_function_prototype_property(receiver, key) {
            self.intrinsic_property_atoms.prototype = Some(key);
            let value = self.ensure_function_prototype(receiver)?;
            return Ok(Some((value, PropertyAttributes::data(true, false, false))));
        }
        Ok(None)
    }

    /// Exposes callable metadata as non-enumerable own virtual data properties.
    fn function_metadata_property(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        let Ok(function) = self.resolve_function_object(receiver) else {
            return Ok(None);
        };
        match function.executable {
            FunctionExecutable::Bound(data) => {
                let metadata = self.bound_function_snapshot(data)?;
                if key == self.length_atom()? {
                    return Ok(Some(metadata.length));
                }
                if key == self.name_atom()? {
                    return Ok(Some(metadata.name));
                }
                Ok(None)
            }
            FunctionExecutable::Native(native) => {
                if key == self.length_atom()? {
                    return Ok(Some(Value::from_i32(native.length())));
                }
                if key != self.name_atom()? {
                    return Ok(None);
                }
                let name = JsString::try_from_latin1(native.name().as_bytes())
                    .map_err(ExecutionError::PropertyKeyString)?;
                self.allocate_runtime_string(name).map(Some)
            }
            FunctionExecutable::Bytecode { code, function, .. } => {
                let is_length = key == self.length_atom()?;
                let is_name = !is_length && key == self.name_atom()?;
                if !is_length && !is_name {
                    return Ok(None);
                }
                let template = self
                    .loaded_code(code)?
                    .module
                    .function(function)
                    .ok_or(ExecutionError::MissingEntryFunction(function))?;
                if is_length {
                    return Ok(Some(Value::from_i32(
                        i32::try_from(template.layout().function_length).unwrap_or(i32::MAX),
                    )));
                }
                let name = template
                    .layout()
                    .name_scope
                    .and_then(|scope| {
                        self.loaded_code(code)
                            .ok()?
                            .module
                            .scope_names()
                            .get(scope as usize)
                    })
                    .map_or("", AsRef::as_ref);
                let name =
                    JsString::try_from_str(name).map_err(ExecutionError::PropertyKeyString)?;
                self.allocate_runtime_string(name).map(Some)
            }
        }
    }

    /// Tests the virtual metadata key without materializing a runtime name string.
    fn is_function_metadata_property(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<bool, ExecutionError> {
        if self.resolve_function_object(receiver).is_err() {
            return Ok(false);
        }
        Ok(key == self.length_atom()? || key == self.name_atom()?)
    }

    /// Parses the supported data fields while preserving absent versus present-undefined.
    fn parse_data_property_descriptor(
        &mut self,
        descriptor: Value,
    ) -> Result<DataPropertyDescriptor, ExecutionError> {
        if !self.is_object_value(descriptor) {
            return Err(ExecutionError::NotObject(descriptor));
        }
        let value_atom = self.intern_intrinsic_name(b"value")?;
        let writable_atom = self.intern_intrinsic_name(b"writable")?;
        let enumerable_atom = self.intern_intrinsic_name(b"enumerable")?;
        let configurable_atom = self.intern_intrinsic_name(b"configurable")?;
        let get_atom = self.intern_intrinsic_name(b"get")?;
        let set_atom = self.intern_intrinsic_name(b"set")?;
        let value = self.get_data_property(descriptor, value_atom)?;
        let writable = self
            .get_data_property(descriptor, writable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let enumerable = self
            .get_data_property(descriptor, enumerable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let configurable = self
            .get_data_property(descriptor, configurable_atom)?
            .map(|value| self.is_truthy_value(value))
            .transpose()?;
        let getter = self.get_data_property(descriptor, get_atom)?;
        let setter = self.get_data_property(descriptor, set_atom)?;
        if getter.is_some() || setter.is_some() {
            return Err(ExecutionError::UnsupportedAccessorDescriptor);
        }
        Ok(DataPropertyDescriptor {
            value,
            writable,
            enumerable,
            configurable,
        })
    }

    /// Defines one spec-facing intrinsic field with non-enumerable builtin attributes.
    fn set_intrinsic_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
        value: Value,
        configurable: bool,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            receiver,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(configurable),
            },
        )
    }

    /// Defines one non-writable, non-enumerable, non-configurable intrinsic constant.
    fn set_intrinsic_constant_property(
        &mut self,
        receiver: Value,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            receiver,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(false),
                enumerable: Some(false),
                configurable: Some(false),
            },
        )
    }

    /// Implements ValidateAndApplyPropertyDescriptor for ordinary data properties.
    fn define_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
        descriptor: DataPropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        if self.is_function_prototype_property(receiver, key) {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let property = self.shapes.lookup(snapshot.shape, key);
        if property.is_none()
            && let Some(current_value) = self.function_metadata_property(receiver, key)?
        {
            let current_attributes = PropertyAttributes::data(false, false, true);
            self.validate_data_property_redefinition(
                receiver,
                current_value,
                current_attributes,
                descriptor,
            )?;
            let attributes = PropertyAttributes::data(
                descriptor.writable.unwrap_or(false),
                descriptor.enumerable.unwrap_or(false),
                descriptor.configurable.unwrap_or(true),
            );
            return self.add_property_slot(
                object,
                snapshot,
                key,
                descriptor.value.unwrap_or(current_value),
                attributes,
            );
        }
        let current = property
            .map(|property| self.property_value_from_snapshot(snapshot, property))
            .transpose()?
            .flatten();
        let Some(current_value) = current else {
            if !snapshot.extensible {
                return Err(ExecutionError::NonExtensibleObject(receiver));
            }
            let attributes = PropertyAttributes::data(
                descriptor.writable.unwrap_or(false),
                descriptor.enumerable.unwrap_or(false),
                descriptor.configurable.unwrap_or(false),
            );
            let value = descriptor
                .value
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            if let Some(property) = property {
                let shape = self
                    .shapes
                    .transition_reconfigure(snapshot.shape, key, attributes)
                    .map_err(ExecutionError::Shape)?;
                self.update_property_slot(snapshot, property.slot, value)?;
                return self.set_object_shape(object, shape);
            }
            return self.add_property_slot(object, snapshot, key, value, attributes);
        };
        let property = property.expect("present property value has shape metadata");
        self.validate_data_property_redefinition(
            receiver,
            current_value,
            property.attributes,
            descriptor,
        )?;
        let attributes = PropertyAttributes::data(
            descriptor
                .writable
                .unwrap_or_else(|| property.attributes.writable()),
            descriptor
                .enumerable
                .unwrap_or_else(|| property.attributes.enumerable()),
            descriptor
                .configurable
                .unwrap_or_else(|| property.attributes.configurable()),
        );
        let shape = self
            .shapes
            .transition_reconfigure(snapshot.shape, key, attributes)
            .map_err(ExecutionError::Shape)?;
        if let Some(value) = descriptor.value {
            self.update_property_slot(snapshot, property.slot, value)?;
        }
        self.set_object_shape(object, shape)
    }

    /// Rejects the immutable combinations required by data descriptor compatibility.
    fn validate_data_property_redefinition(
        &mut self,
        receiver: Value,
        current_value: Value,
        current: PropertyAttributes,
        descriptor: DataPropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        if current.configurable() {
            return Ok(());
        }
        if descriptor.configurable == Some(true)
            || descriptor
                .enumerable
                .is_some_and(|enumerable| enumerable != current.enumerable())
            || (!current.writable() && descriptor.writable == Some(true))
        {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        if !current.writable()
            && let Some(value) = descriptor.value
            && !self.same_value(value, current_value)?
        {
            return Err(ExecutionError::InvalidPropertyRedefinition(receiver));
        }
        Ok(())
    }

    /// Populates a rooted fresh object with the four standard data descriptor fields.
    fn materialize_data_property_descriptor(
        &mut self,
        result: Value,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        let value_atom = self.intern_intrinsic_name(b"value")?;
        self.set_own_data_property(result, value_atom, value)?;
        let writable_atom = self.intern_intrinsic_name(b"writable")?;
        self.set_own_data_property(
            result,
            writable_atom,
            Value::from_immediate(if attributes.writable() {
                Immediate::True
            } else {
                Immediate::False
            }),
        )?;
        let enumerable_atom = self.intern_intrinsic_name(b"enumerable")?;
        self.set_own_data_property(
            result,
            enumerable_atom,
            Value::from_immediate(if attributes.enumerable() {
                Immediate::True
            } else {
                Immediate::False
            }),
        )?;
        let configurable_atom = self.intern_intrinsic_name(b"configurable")?;
        self.set_own_data_property(
            result,
            configurable_atom,
            Value::from_immediate(if attributes.configurable() {
                Immediate::True
            } else {
                Immediate::False
            }),
        )
    }

    /// Snapshots the currently visible enumerable string keys into one managed iterator payload.
    fn create_for_in_iterator(&mut self, source: Value) -> Result<Value, ExecutionError> {
        let keys = self.for_in_keys(source)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.for_in_iterator,
                0,
                ForInIterator::new(keys),
                AllocationSpace::Young,
                roots,
            )
            .map(|iterator| Value::from_heap_ref(iterator.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Applies ordinary `for-in` shadowing: every present own key suppresses prototypes.
    fn for_in_keys(&mut self, source: Value) -> Result<Box<[AtomId]>, ExecutionError> {
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Ok(Box::default());
        }
        if let Some(raw) = source.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.for_in_string_keys(string);
        }
        if !self.is_object_value(source) {
            return Ok(Box::default());
        }
        let upper_bound = self.for_in_object_key_upper_bound(source)?;
        let mut keys = ForInKeySet::with_upper_bound(upper_bound)
            .map_err(|_: ForInAllocationError| ExecutionError::ForInKeyAllocationFailed)?;
        let mut current = source;
        loop {
            self.insert_for_in_virtual_function_keys(current, &mut keys)?;
            let (_, snapshot) = self.object_snapshot(current)?;
            for key in self
                .shapes
                .own_keys(snapshot.shape)
                .map_err(ExecutionError::Shape)?
            {
                let property = self
                    .shapes
                    .lookup(snapshot.shape, key)
                    .expect("own key resolves in its source shape");
                if self
                    .property_value_from_snapshot(snapshot, property)?
                    .is_some()
                    && keys.insert(key)
                    && property.attributes.enumerable()
                {
                    keys.push_enumerable(key);
                }
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                break;
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
        Ok(keys.finish())
    }

    /// Counts shape and virtual function keys before collection so snapshot vectors never grow.
    fn for_in_object_key_upper_bound(&mut self, source: Value) -> Result<usize, ExecutionError> {
        let mut count = 0_usize;
        let mut current = source;
        loop {
            let virtual_count = match self.resolve_function_object(current) {
                Ok(function) => match function.executable {
                    FunctionExecutable::Native(_) => 3,
                    FunctionExecutable::Bound(_) => 2,
                    FunctionExecutable::Bytecode { .. } => 3,
                },
                Err(_) => 0,
            };
            let (_, snapshot) = self.object_snapshot(current)?;
            count = count
                .checked_add(virtual_count)
                .and_then(|count| {
                    usize::try_from(self.shapes.property_count(snapshot.shape))
                        .ok()
                        .and_then(|properties| count.checked_add(properties))
                })
                .ok_or(ExecutionError::ForInKeyAllocationFailed)?;
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(count);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Adds non-enumerable virtual function fields to the shadow set without materializing values.
    fn insert_for_in_virtual_function_keys(
        &mut self,
        receiver: Value,
        keys: &mut ForInKeySet,
    ) -> Result<(), ExecutionError> {
        if self.resolve_function_object(receiver).is_err() {
            return Ok(());
        }
        let (_, snapshot) = self.object_snapshot(receiver)?;
        let name = self.name_atom()?;
        if self.shapes.lookup(snapshot.shape, name).is_none() {
            keys.insert(name);
        }
        let length = self.length_atom()?;
        if self.shapes.lookup(snapshot.shape, length).is_none() {
            keys.insert(length);
        }
        let prototype = self.prototype_atom()?;
        if self.shapes.lookup(snapshot.shape, prototype).is_none()
            && self.is_function_prototype_property(receiver, prototype)
        {
            keys.insert(prototype);
        }
        Ok(())
    }

    /// Enumerates primitive string indices without retaining copies of their character values.
    fn for_in_string_keys(
        &mut self,
        string: GcRef<JsString>,
    ) -> Result<Box<[AtomId]>, ExecutionError> {
        let length = self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(|string| string.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(length)
            .map_err(|_| ExecutionError::ForInKeyAllocationFailed)?;
        for index in 0..length {
            let index =
                i32::try_from(index).map_err(|_| ExecutionError::ForInKeyAllocationFailed)?;
            keys.push(self.property_key_atom(Value::from_i32(index))?);
        }
        Ok(keys.into_boxed_slice())
    }

    /// Advances one verified internal iterator and materializes only the returned atom string.
    fn for_in_next(&mut self, iterator: Value) -> Result<Value, ExecutionError> {
        let raw = iterator
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidForInIterator(iterator))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.for_in_iterator)
            .map_err(|_| ExecutionError::InvalidForInIterator(iterator))?;
        let key = self.heap.with_running_scope(|scope| {
            let iterator = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(iterator, self.types.for_in_iterator)
                    .map(ForInIterator::next)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        key.map_or_else(
            || Ok(Value::from_immediate(Immediate::Undefined)),
            |key| self.atom_string_value(key),
        )
    }

    /// Reads a known ordinary snapshot's fixed slot without repeating receiver classification.
    fn data_property_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        key: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
            return Ok(None);
        };
        self.property_value_from_snapshot(snapshot, property)
    }

    /// Reads one resolved fixed slot and maps the retained deletion sentinel back to absence.
    fn property_value_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        property: PropertyLookup,
    ) -> Result<Option<Value>, ExecutionError> {
        let storage = snapshot
            .storage
            .expect("a non-empty shape always owns property storage");
        self.heap.with_running_scope(|scope| {
            let local = scope.root(storage).map_err(ExecutionError::Root)?;
            scope
                .with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.property_storage)
                        .map_err(ExecutionError::NoGcBorrow)
                        .map(|storage| storage.slots.get(property.slot as usize).copied())
                })
                .map(|value| value.filter(|value| value.as_immediate() != Some(Immediate::Hole)))
        })
    }

    fn prototype_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.prototype {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"prototype").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.prototype = Some(atom);
        Ok(atom)
    }

    fn constructor_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.constructor {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"constructor").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.constructor = Some(atom);
        Ok(atom)
    }

    fn message_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.message {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"message").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.message = Some(atom);
        Ok(atom)
    }

    fn name_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.name {
            return Ok(atom);
        }
        let atom = self.intern_intrinsic_name(b"name")?;
        self.intrinsic_property_atoms.name = Some(atom);
        Ok(atom)
    }

    fn length_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.length {
            return Ok(atom);
        }
        let atom = self.intern_intrinsic_name(b"length")?;
        self.intrinsic_property_atoms.length = Some(atom);
        Ok(atom)
    }

    /// Allocates one ordinary native error and defines a string message only when supplied.
    fn create_native_error(
        &mut self,
        kind: NativeErrorKind,
        message: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .error_intrinsics
            .get(kind)
            .prototype
            .expect("native Error prototypes initialize before execution");
        let error = self.create_ordinary_object_with_prototype(prototype)?;
        let Some(message) =
            message.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(error);
        };
        let raw = message
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedErrorMessage(message))?;
        self.heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedErrorMessage(message))?;
        let message_atom = self.message_atom()?;
        self.set_own_data_property(error, message_atom, message)?;
        Ok(error)
    }

    #[inline(always)]
    fn is_function_prototype_property(&mut self, receiver: Value, key: AtomId) -> bool {
        let is_prototype_name = self.intrinsic_property_atoms.prototype == Some(key)
            || self
                .atoms
                .get(key)
                .is_some_and(|name| name.equals_latin1(b"prototype"));
        if !is_prototype_name {
            return false;
        }
        self.resolve_function_object(receiver)
            .is_ok_and(|function| match function.executable {
                FunctionExecutable::Bytecode { .. } => true,
                FunctionExecutable::Native(native) => native.is_constructor(),
                FunctionExecutable::Bound(_) => false,
            })
    }

    /// Materializes the spec-visible function prototype only on first observation or construction.
    fn ensure_function_prototype(&mut self, function: Value) -> Result<Value, ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        let existing = self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(function, self.types.function)
                    .map(|function| function.function_prototype)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if let Some(prototype) = existing {
            return Ok(prototype);
        }
        self.materialize_function_prototype(function)
    }

    /// Allocates a one-slot constructor object, then publishes the lazy function edge with a barrier.
    fn materialize_function_prototype(&mut self, function: Value) -> Result<Value, ExecutionError> {
        let constructor_atom = self.constructor_atom()?;
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                constructor_atom,
                PropertyAttributes::DEFAULT_DATA,
            )
            .map_err(ExecutionError::Shape)?;
        let mut roots = PrototypeInitializationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            function,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage {
                    slots: Box::new([roots.function]),
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let prototype = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape,
                    extensible: true,
                    storage: Some(storage),
                    prototype: Value::from_immediate(Immediate::Null),
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let function = roots.function;
        self.set_function_prototype(function, Value::from_heap_ref(prototype.raw()))?;
        Ok(Value::from_heap_ref(prototype.raw()))
    }

    /// Replaces the inline function prototype slot and records its possible young edge.
    fn set_function_prototype(
        &mut self,
        function: Value,
        prototype: Value,
    ) -> Result<(), ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(function, self.types.function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.function_prototype = Some(prototype);
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(function, prototype)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Replaces one callable's ordinary `[[Prototype]]` edge and publishes the GC barrier.
    fn set_function_internal_prototype(
        &mut self,
        function: Value,
        prototype: Value,
    ) -> Result<(), ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(function, self.types.function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.ordinary.prototype = prototype;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(function, prototype)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Implements ordinary HasInstance over the current constructor prototype and object chain.
    fn ordinary_instance_of(
        &mut self,
        value: Value,
        mut constructor: Value,
    ) -> Result<bool, ExecutionError> {
        loop {
            let function = self.resolve_function_object(constructor)?;
            let FunctionExecutable::Bound(data) = function.executable else {
                break;
            };
            constructor = self.bound_function_snapshot(data)?.call_target;
        }
        let prototype_atom = self.prototype_atom()?;
        let prototype = self.get_data_property(constructor, prototype_atom)?.ok_or(
            ExecutionError::InvalidInstanceofPrototype(Value::from_immediate(Immediate::Undefined)),
        )?;
        if !self.is_object_value(prototype) {
            return Err(ExecutionError::InvalidInstanceofPrototype(prototype));
        }
        if !self.is_object_value(value) {
            return Ok(false);
        }
        let (_, mut snapshot) = self.object_snapshot(value)?;
        loop {
            let candidate = snapshot.prototype;
            if candidate.as_immediate() == Some(Immediate::Null) {
                return Ok(false);
            }
            if candidate == prototype {
                return Ok(true);
            }
            let (_, next) = self.object_snapshot(candidate)?;
            snapshot = next;
        }
    }

    /// Updates an existing slot in place or publishes an exactly sized replacement backing.
    fn set_own_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_function_prototype_property(receiver, key) {
            self.intrinsic_property_atoms.prototype = Some(key);
            return self.set_function_prototype(receiver, value);
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
            if self
                .property_value_from_snapshot(snapshot, property)?
                .is_some()
            {
                if !property.attributes.writable() {
                    return Err(ExecutionError::ReadOnlyProperty(receiver));
                }
                return self.update_property_slot(snapshot, property.slot, value);
            }
            if !snapshot.extensible {
                return Err(ExecutionError::NonExtensibleObject(receiver));
            }
            let shape = self
                .shapes
                .transition_reconfigure(snapshot.shape, key, PropertyAttributes::DEFAULT_DATA)
                .map_err(ExecutionError::Shape)?;
            self.set_object_shape(object, shape)?;
            return self.update_property_slot(snapshot, property.slot, value);
        }
        if self.is_function_metadata_property(receiver, key)? {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        if !snapshot.extensible {
            return Err(ExecutionError::NonExtensibleObject(receiver));
        }
        self.add_property_slot(
            object,
            snapshot,
            key,
            value,
            PropertyAttributes::DEFAULT_DATA,
        )
    }

    /// Applies assignment failure semantics without weakening throwing descriptor operations.
    fn set_data_property_from_bytecode(
        &mut self,
        receiver: Value,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let strictness = self
            .fiber
            .frames
            .last()
            .expect("property assignment always has an active frame")
            .strictness;
        match self.set_own_data_property(receiver, key, value) {
            Err(ExecutionError::NonExtensibleObject(_) | ExecutionError::ReadOnlyProperty(_))
                if strictness == FunctionStrictness::Sloppy =>
            {
                Ok(())
            }
            result => result,
        }
    }

    /// Marks one own data property as deleted while retaining append-only shape metadata.
    fn delete_own_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<bool, ExecutionError> {
        let (object, snapshot) = self.object_snapshot(receiver)?;
        let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
            if self.is_function_prototype_property(receiver, key) {
                return Ok(false);
            }
            if self.is_function_metadata_property(receiver, key)? {
                self.add_property_slot(
                    object,
                    snapshot,
                    key,
                    Value::from_immediate(Immediate::Hole),
                    PropertyAttributes::data(false, false, true),
                )?;
            }
            return Ok(true);
        };
        if !property.attributes.configurable() {
            return Ok(false);
        }
        self.update_property_slot(
            snapshot,
            property.slot,
            Value::from_immediate(Immediate::Hole),
        )?;
        Ok(true)
    }

    /// Applies strict DeletePropertyOrThrow semantics to bytecode property deletion.
    fn delete_data_property_from_bytecode(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<bool, ExecutionError> {
        let deleted = self.delete_own_data_property(receiver, key)?;
        let strictness = self
            .fiber
            .frames
            .last()
            .expect("property deletion always has an active frame")
            .strictness;
        if !deleted && strictness == FunctionStrictness::Strict {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        Ok(deleted)
    }

    /// Mutates a fixed existing slot and publishes its potential young edge to the barrier.
    fn update_property_slot(
        &mut self,
        snapshot: OrdinaryObject,
        slot: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let storage = snapshot
            .storage
            .expect("an existing property slot always has storage");
        self.heap.with_running_scope(|scope| {
            let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let storage = no_gc
                    .borrow_mut(storage_local, self.types.property_storage)
                    .map_err(ExecutionError::NoGcBorrow)?;
                storage.slots[slot as usize] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(storage_local, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Copies old slots into a traced pending backing, allocates it, then switches the object edge.
    fn add_property_slot(
        &mut self,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: AtomId,
        value: Value,
        attributes: PropertyAttributes,
    ) -> Result<(), ExecutionError> {
        let new_shape = self
            .shapes
            .transition_add(snapshot.shape, key, attributes)
            .map_err(ExecutionError::Shape)?;
        let new_length = self.shapes.property_count(new_shape) as usize;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(new_length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        if let Some(storage) = snapshot.storage {
            self.heap.with_running_scope(|scope| {
                let local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let old = no_gc
                        .borrow(local, self.types.property_storage)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    slots.extend_from_slice(&old.slots);
                    Ok::<(), ExecutionError>(())
                })
            })?;
        }
        slots.push(value);
        debug_assert_eq!(slots.len(), new_length);
        let (storage, receiver) = {
            let mut roots = PropertyMutationRoots {
                vm: VmRoots {
                    fiber: &mut self.fiber,
                    finalization_jobs: &mut self.finalization_jobs,
                    realm: &mut self.realm,
                    loaded_code: &mut self.loaded_code,
                },
                receiver: object.value(),
            };
            let storage = self
                .heap
                .try_allocate_external_with_gc(
                    self.types.property_storage,
                    0,
                    PropertyStorage {
                        slots: slots.into_boxed_slice(),
                    },
                    AllocationSpace::Young,
                    &mut roots,
                )
                .map_err(ExecutionError::HeapAllocation)?;
            (storage, roots.receiver)
        };
        let (object, _) = self.object_snapshot(receiver)?;
        self.attach_property_storage(object, new_shape, storage)
    }

    /// Resolves either ordinary or callable payloads to their shared ordinary-property snapshot.
    #[inline(always)]
    fn object_snapshot(
        &mut self,
        value: Value,
    ) -> Result<(ObjectReceiver, OrdinaryObject), ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        if let Ok(object) = self.heap.checked_reference(raw, self.types.ordinary_object) {
            let snapshot = self.heap.with_running_scope(|scope| {
                let local = scope.root(object).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.ordinary_object)
                        .copied()
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Ordinary(object), snapshot));
        }
        if let Ok(array) = self.heap.checked_reference(raw, self.types.array) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.array)
                        .map(|array| array.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Array(array), ordinary));
        }
        if let Ok(number) = self.heap.checked_reference(raw, self.types.number_object) {
            let ordinary = self.heap.with_running_scope(|scope| {
                let local = scope.root(number).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.number_object)
                        .map(|number| number.ordinary)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Number(number), ordinary));
        }
        let function = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NotObject(value))?;
        let ordinary = self.heap.with_running_scope(|scope| {
            let local = scope.root(function).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(local, self.types.function)
                    .map(|function| function.ordinary)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok((ObjectReceiver::Function(function), ordinary))
    }

    /// Mutates the shared ordinary-object state for either object payload representation.
    fn set_object_extensible(
        &mut self,
        receiver: ObjectReceiver,
        extensible: bool,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Array(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(array, self.types.array)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
            ObjectReceiver::Number(number) => self.heap.with_running_scope(|scope| {
                let number = scope.root(number).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(number, self.types.number_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .extensible = extensible;
                    Ok(())
                })
            }),
        }
    }

    /// Switches immutable shape metadata without touching the unchanged storage edge.
    fn set_object_shape(
        &mut self,
        receiver: ObjectReceiver,
        shape: ShapeId,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Array(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(array, self.types.array)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
            ObjectReceiver::Number(number) => self.heap.with_running_scope(|scope| {
                let number = scope.root(number).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(number, self.types.number_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .ordinary
                        .shape = shape;
                    Ok(())
                })
            }),
        }
    }

    /// Publishes a replacement storage edge through the receiver's concrete typed payload.
    fn attach_property_storage(
        &mut self,
        receiver: ObjectReceiver,
        shape: ShapeId,
        storage: GcRef<PropertyStorage>,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let object = no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    object.shape = shape;
                    object.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(object, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
            ObjectReceiver::Array(array) => self.heap.with_running_scope(|scope| {
                let array = scope.root(array).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let array = no_gc
                        .borrow_mut(array, self.types.array)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    array.ordinary.shape = shape;
                    array.ordinary.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(array, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let function = no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    function.ordinary.shape = shape;
                    function.ordinary.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(function, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
            ObjectReceiver::Number(number) => self.heap.with_running_scope(|scope| {
                let number = scope.root(number).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let number = no_gc
                        .borrow_mut(number, self.types.number_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    number.ordinary.shape = shape;
                    number.ordinary.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(number, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
        }
    }

    #[inline(always)]
    fn is_object_value(&self, value: Value) -> bool {
        let Some(raw) = value.as_heap_ref() else {
            return false;
        };
        self.heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
            || self.heap.checked_reference(raw, self.types.array).is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.number_object)
                .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.function)
                .is_ok()
    }
}

impl Trace for Isolate {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.trace_roots(tracer);
    }
}

#[inline(always)]
fn execution_error_kind(error: &ExecutionError) -> Option<NativeErrorKind> {
    match error {
        ExecutionError::UnresolvedBinding(_) | ExecutionError::UninitializedBinding(_) => {
            Some(NativeErrorKind::Reference)
        }
        ExecutionError::NonCallable(_)
        | ExecutionError::NonConstructor(_)
        | ExecutionError::InvalidInstanceofPrototype(_)
        | ExecutionError::ReadOnlyBinding(_)
        | ExecutionError::ImmutableBinding(_)
        | ExecutionError::NonExtensibleObject(_)
        | ExecutionError::ReadOnlyProperty(_)
        | ExecutionError::InvalidPropertyRedefinition(_)
        | ExecutionError::ArrayLengthOverflow
        | ExecutionError::NotObject(_) => Some(NativeErrorKind::Type),
        ExecutionError::GlobalLexicalRedeclaration(_)
        | ExecutionError::GlobalLexicalAlreadyInitialized(_) => Some(NativeErrorKind::Syntax),
        ExecutionError::InvalidNumberRadix(_) | ExecutionError::InvalidNumberPrecision(_) => {
            Some(NativeErrorKind::Range)
        }
        _ => None,
    }
}

struct Int32PropertyKey {
    bytes: [u8; 11],
    start: u8,
}

impl Int32PropertyKey {
    /// Formats every i32 PropertyKey into owned stack storage without Rust formatting machinery.
    fn new(value: i32) -> Self {
        let mut bytes = [0_u8; 11];
        let mut start = bytes.len();
        let mut magnitude = value.unsigned_abs();
        loop {
            start -= 1;
            bytes[start] = b'0' + (magnitude % 10) as u8;
            magnitude /= 10;
            if magnitude == 0 {
                break;
            }
        }
        if value < 0 {
            start -= 1;
            bytes[start] = b'-';
        }
        Self {
            bytes,
            start: start as u8,
        }
    }

    #[inline(always)]
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[self.start as usize..]
    }
}

/// Executes allocation-free success branches while the verified cursor remains in local state.
///
/// # Safety
///
/// `instruction` must come from the verified function used to create `registers`; the register
/// backing must remain exclusively owned and must not change allocation or length until this
/// function returns. A `Slow` result ends that epoch before general isolate code runs.
#[inline(always)]
unsafe fn execute_verified_hot_instruction(
    registers: &mut RegisterWindow,
    instruction: DecodedInstruction,
    next_pc: &mut WordOffset,
) -> HotControl {
    let operands = instruction.operands;
    // SAFETY: The caller proves the verified operand and no-reallocation cursor invariants once for
    // this operation. Reads return Copy values and writes never expose references beyond the epoch.
    unsafe {
        match instruction.opcode {
            Opcode::Nop => HotControl::Continue,
            Opcode::LoadUndefined => {
                registers.write(operands[0], Value::from_immediate(Immediate::Undefined));
                HotControl::Continue
            }
            Opcode::LoadNull => {
                registers.write(operands[0], Value::from_immediate(Immediate::Null));
                HotControl::Continue
            }
            Opcode::LoadFalse => {
                registers.write(operands[0], Value::from_immediate(Immediate::False));
                HotControl::Continue
            }
            Opcode::LoadTrue => {
                registers.write(operands[0], Value::from_immediate(Immediate::True));
                HotControl::Continue
            }
            Opcode::LoadImmediate => {
                registers.write(operands[0], Value::from_i32(operands[1] as i32));
                HotControl::Continue
            }
            Opcode::Move => {
                let value = registers.read(operands[1]);
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::Not => {
                let input = registers.read(operands[1]);
                if input.as_heap_ref().is_some() {
                    return HotControl::Slow;
                }
                let value = if is_non_string_truthy(input) {
                    Immediate::False
                } else {
                    Immediate::True
                };
                registers.write(operands[0], Value::from_immediate(value));
                HotControl::Continue
            }
            Opcode::Negate | Opcode::BitwiseNot | Opcode::ToNumber => {
                let input = registers.read(operands[1]);
                if numeric_value(input).is_none() {
                    return HotControl::Slow;
                }
                let value = match instruction.opcode {
                    Opcode::Negate => numeric_negate(input),
                    Opcode::BitwiseNot => numeric_bitwise_not(input),
                    Opcode::ToNumber => input,
                    _ => unreachable!("numeric unary hot dispatch is exhaustive"),
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::Add => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(value) = numeric_binary_hot(Opcode::Add, left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::Sub | Opcode::Mul | Opcode::Div => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(value) = numeric_binary_hot(instruction.opcode, left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::BitwiseAnd
            | Opcode::BitwiseOr
            | Opcode::BitwiseXor
            | Opcode::ShiftLeft
            | Opcode::ShiftRight
            | Opcode::ShiftRightUnsigned
            | Opcode::Remainder
            | Opcode::Exponentiate => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                if numeric_value(left).is_none() || numeric_value(right).is_none() {
                    return HotControl::Slow;
                }
                registers.write(
                    operands[0],
                    numeric_binary_operation(instruction.opcode, left, right),
                );
                HotControl::Continue
            }
            Opcode::LessThan | Opcode::GreaterThan | Opcode::LessEqual | Opcode::GreaterEqual => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(value) = numeric_relational_hot(instruction.opcode, left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::StrictEqual => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(equal) = strict_equal_hot(left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], boolean_value(equal));
                HotControl::Continue
            }
            Opcode::Jump => {
                *next_pc = WordOffset::new(operands[0]);
                HotControl::Continue
            }
            Opcode::JumpIfFalse | Opcode::JumpIfTrue => {
                let condition = registers.read(operands[0]);
                if condition.as_heap_ref().is_some() {
                    return HotControl::Slow;
                }
                let truthy = is_non_string_truthy(condition);
                if truthy == (instruction.opcode == Opcode::JumpIfTrue) {
                    *next_pc = WordOffset::new(operands[1]);
                }
                HotControl::Continue
            }
            Opcode::JumpIfNotNullish => {
                if !is_nullish(registers.read(operands[0])) {
                    *next_pc = WordOffset::new(operands[1]);
                }
                HotControl::Continue
            }
            _ => HotControl::Slow,
        }
    }
}

#[cfg(test)]
mod tests;
