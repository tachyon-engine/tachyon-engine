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
mod finalization;
mod for_in;
mod number;
mod object;
mod string;
mod tuning;

pub use atom::{AtomHashSeed, AtomId, AtomTable, AtomTableConfig, AtomTableError, AtomTableStats};

pub use finalization::{
    FinalizationCleanupJob, FinalizationJobQueueStats, FinalizationSafepointError,
    FinalizationSafepointStats,
};
pub use object::ShapeError;
pub use string::{JsString, JsStringView, StringAllocationError, StringRepresentationTag};

use core::{cell::Cell, num::NonZeroU32, ptr::NonNull};

use tachyon_bytecode::{
    BytecodeConstant, CompiledModule, DecodeError, DecodedInstruction, FunctionId, FunctionKind,
    FunctionLayout, FunctionStrictness, HandlerEntry, HandlerKind, Opcode, RegisterId, WordOffset,
    decode_instruction,
};
use tachyon_gc::{
    AllocationSpace, GcExternalMemory, GcRef, GcType, Heap, HeapAllocationError, HeapLimit,
    HeapReferenceError, ManagedAllocationError, NoGcBorrowError, RootError, Trace, Tracer,
    TypeRegistrationError, TypeRegistry,
};
use tachyon_value::{Immediate, Value};

use array::{ArrayObject, MAX_SAFE_INTEGER};
use bound_function::BoundFunctionData;
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
    words: NonNull<u32>,
    word_count: usize,
}

impl BytecodeCursor {
    /// Captures stable `Arc<[u32]>` backing without incrementing its reference count per batch.
    fn new(words: &[u32]) -> Self {
        debug_assert!(
            !words.is_empty(),
            "verified functions contain a terminal opcode"
        );
        Self {
            words: NonNull::new(words.as_ptr().cast_mut())
                .expect("slice pointers are non-null even for empty slices"),
            word_count: words.len(),
        }
    }

    /// Decodes while the dispatch owner retains the immutable module which owns this allocation.
    #[inline(always)]
    fn decode(self, offset: WordOffset) -> Result<DecodedInstruction, DecodeError> {
        // SAFETY: `execute_batch` creates the cursor from the active `LoadedCode` module. Loaded
        // modules are append-only and `CompiledModule` owns the words through an immutable Arc, so
        // dispatch may mutate isolate state or grow the loaded-code Vec without moving/freeing this
        // backing. Frame identity is checked before every decode and the cursor never escapes a batch.
        let words = unsafe { core::slice::from_raw_parts(self.words.as_ptr(), self.word_count) };
        decode_instruction(words, offset)
    }
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
            | Self::BinaryRight(_) => None,
        }
    }

    #[inline]
    const fn uses_string_hint(self) -> bool {
        matches!(self, Self::NativeCall(NativeFunction::StringConstructor))
    }

    #[inline]
    const fn is_numeric_operation(self) -> bool {
        matches!(
            self,
            Self::ToNumber
                | Self::Negate
                | Self::BitwiseNot
                | Self::BinaryLeft(_)
                | Self::BinaryRight(_)
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

    /// Builds the object/function prototype graph and intrinsic constructors before publication.
    fn initialize_realm_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let object_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: Value::from_immediate(Immediate::Null),
        })?;
        self.realm.object_prototype = Some(object_prototype);
        let global_object = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        self.realm.global_object = Some(global_object);
        self.initialize_function_intrinsics()?;
        self.initialize_object_intrinsics()?;
        self.initialize_primitive_constructors()?;
        let atoms = self.intern_realm_intrinsic_atoms()?;
        self.initialize_error_intrinsics()?;
        self.initialize_array_intrinsics()?;
        self.initialize_math_intrinsics()?;
        self.publish_realm_intrinsic_bindings(atoms)
    }

    /// Builds Object constructor, Object.prototype, and the basic own-property native methods.
    fn initialize_object_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before Object constructor");
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Object constructor");
        let constructor = self.allocate_native_function(
            NativeFunction::ObjectConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_constructor = Some(constructor);
        self.set_function_prototype(constructor, object_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(object_prototype, constructor_atom, constructor, true)?;
        let define_property = self.allocate_native_function(
            NativeFunction::ObjectDefineProperty,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_define_property = Some(define_property);
        let define_atom = self.intern_intrinsic_name(b"defineProperty")?;
        self.set_intrinsic_data_property(constructor, define_atom, define_property, true)?;
        let get_own_property_descriptor = self.allocate_native_function(
            NativeFunction::ObjectGetOwnPropertyDescriptor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_get_own_property_descriptor = Some(get_own_property_descriptor);
        let get_own_descriptor_atom = self.intern_intrinsic_name(b"getOwnPropertyDescriptor")?;
        self.set_intrinsic_data_property(
            constructor,
            get_own_descriptor_atom,
            get_own_property_descriptor,
            true,
        )?;
        let get_own_property_names = self.allocate_native_function(
            NativeFunction::ObjectGetOwnPropertyNames,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_get_own_property_names = Some(get_own_property_names);
        let get_own_names_atom = self.intern_intrinsic_name(b"getOwnPropertyNames")?;
        self.set_intrinsic_data_property(
            constructor,
            get_own_names_atom,
            get_own_property_names,
            true,
        )?;
        let has_own_property = self.allocate_native_function(
            NativeFunction::ObjectHasOwnProperty,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_has_own_property = Some(has_own_property);
        let has_own_atom = self.intern_intrinsic_name(b"hasOwnProperty")?;
        self.set_intrinsic_data_property(object_prototype, has_own_atom, has_own_property, true)?;
        let property_is_enumerable = self.allocate_native_function(
            NativeFunction::ObjectPropertyIsEnumerable,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_property_is_enumerable = Some(property_is_enumerable);
        let property_is_enumerable_atom = self.intern_intrinsic_name(b"propertyIsEnumerable")?;
        self.set_intrinsic_data_property(
            object_prototype,
            property_is_enumerable_atom,
            property_is_enumerable,
            true,
        )?;
        let to_string = self.allocate_native_function(
            NativeFunction::ObjectToString,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_to_string = Some(to_string);
        let to_string_atom = self.intern_intrinsic_name(b"toString")?;
        self.set_intrinsic_data_property(object_prototype, to_string_atom, to_string, true)?;
        let assign = self.allocate_native_function(
            NativeFunction::ObjectAssign,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_assign = Some(assign);
        let assign_atom = self.intern_intrinsic_name(b"assign")?;
        self.set_intrinsic_data_property(constructor, assign_atom, assign, true)?;
        let keys = self.allocate_native_function(
            NativeFunction::ObjectKeys,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_keys = Some(keys);
        let keys_atom = self.intern_intrinsic_name(b"keys")?;
        self.set_intrinsic_data_property(constructor, keys_atom, keys, true)?;
        let values = self.allocate_native_function(
            NativeFunction::ObjectValues,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_values = Some(values);
        let values_atom = self.intern_intrinsic_name(b"values")?;
        self.set_intrinsic_data_property(constructor, values_atom, values, true)?;
        let entries = self.allocate_native_function(
            NativeFunction::ObjectEntries,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_entries = Some(entries);
        let entries_atom = self.intern_intrinsic_name(b"entries")?;
        self.set_intrinsic_data_property(constructor, entries_atom, entries, true)?;
        let has_own = self.allocate_native_function(
            NativeFunction::ObjectHasOwn,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_has_own = Some(has_own);
        let has_own_atom = self.intern_intrinsic_name(b"hasOwn")?;
        self.set_intrinsic_data_property(constructor, has_own_atom, has_own, true)?;
        let object_is = self.allocate_native_function(
            NativeFunction::ObjectIs,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_is = Some(object_is);
        let is_atom = self.intern_intrinsic_name(b"is")?;
        self.set_intrinsic_data_property(constructor, is_atom, object_is, true)?;
        let get_prototype_of = self.allocate_native_function(
            NativeFunction::ObjectGetPrototypeOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_get_prototype_of = Some(get_prototype_of);
        let get_prototype_atom = self.intern_intrinsic_name(b"getPrototypeOf")?;
        self.set_intrinsic_data_property(constructor, get_prototype_atom, get_prototype_of, true)?;
        let create = self.allocate_native_function(
            NativeFunction::ObjectCreate,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_create = Some(create);
        let create_atom = self.intern_intrinsic_name(b"create")?;
        self.set_intrinsic_data_property(constructor, create_atom, create, true)?;
        let is_prototype_of = self.allocate_native_function(
            NativeFunction::ObjectIsPrototypeOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_is_prototype_of = Some(is_prototype_of);
        let is_prototype_atom = self.intern_intrinsic_name(b"isPrototypeOf")?;
        self.set_intrinsic_data_property(
            object_prototype,
            is_prototype_atom,
            is_prototype_of,
            true,
        )?;
        let is_extensible = self.allocate_native_function(
            NativeFunction::ObjectIsExtensible,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_is_extensible = Some(is_extensible);
        let is_extensible_atom = self.intern_intrinsic_name(b"isExtensible")?;
        self.set_intrinsic_data_property(constructor, is_extensible_atom, is_extensible, true)?;
        let prevent_extensions = self.allocate_native_function(
            NativeFunction::ObjectPreventExtensions,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_prevent_extensions = Some(prevent_extensions);
        let prevent_extensions_atom = self.intern_intrinsic_name(b"preventExtensions")?;
        self.set_intrinsic_data_property(
            constructor,
            prevent_extensions_atom,
            prevent_extensions,
            true,
        )
    }

    /// Builds primitive conversion constructors with the shared callable prototype.
    fn initialize_primitive_constructors(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before primitive constructors");
        let allocate = |this: &mut Self, native: NativeFunction| -> Result<Value, ExecutionError> {
            this.allocate_native_function(
                native,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )
        };
        self.realm.string_constructor = Some(allocate(self, NativeFunction::StringConstructor)?);
        self.realm.symbol_constructor = Some(allocate(self, NativeFunction::SymbolConstructor)?);
        let number = allocate(self, NativeFunction::NumberConstructor)?;
        self.realm.number_constructor = Some(number);
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Number prototype");
        let number_prototype = self.allocate_number_object(
            Value::from_i32(0),
            object_prototype,
            AllocationSpace::Old,
        )?;
        self.realm.number_prototype = Some(number_prototype);
        self.set_function_prototype(number, number_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(number_prototype, constructor_atom, number, true)?;
        let to_exponential = allocate(self, NativeFunction::NumberToExponential)?;
        self.realm.number_to_exponential = Some(to_exponential);
        let to_exponential_atom = self.intern_intrinsic_name(b"toExponential")?;
        self.set_intrinsic_data_property(
            number_prototype,
            to_exponential_atom,
            to_exponential,
            true,
        )?;
        let to_fixed = allocate(self, NativeFunction::NumberToFixed)?;
        self.realm.number_to_fixed = Some(to_fixed);
        let to_fixed_atom = self.intern_intrinsic_name(b"toFixed")?;
        self.set_intrinsic_data_property(number_prototype, to_fixed_atom, to_fixed, true)?;
        let to_precision = allocate(self, NativeFunction::NumberToPrecision)?;
        self.realm.number_to_precision = Some(to_precision);
        let to_precision_atom = self.intern_intrinsic_name(b"toPrecision")?;
        self.set_intrinsic_data_property(number_prototype, to_precision_atom, to_precision, true)?;
        let to_string = allocate(self, NativeFunction::NumberToString)?;
        self.realm.number_to_string = Some(to_string);
        let to_string_atom = self.intern_intrinsic_name(b"toString")?;
        self.set_intrinsic_data_property(number_prototype, to_string_atom, to_string, true)?;
        let value_of = allocate(self, NativeFunction::NumberValueOf)?;
        self.realm.number_value_of = Some(value_of);
        let value_of_atom = self.intern_intrinsic_name(b"valueOf")?;
        self.set_intrinsic_data_property(number_prototype, value_of_atom, value_of, true)?;
        let is_nan = allocate(self, NativeFunction::NumberIsNaN)?;
        self.realm.number_is_nan = Some(is_nan);
        let is_nan_atom = self.intern_intrinsic_name(b"isNaN")?;
        self.set_intrinsic_data_property(number, is_nan_atom, is_nan, true)?;
        let is_finite = allocate(self, NativeFunction::NumberIsFinite)?;
        self.realm.number_is_finite = Some(is_finite);
        let is_finite_atom = self.intern_intrinsic_name(b"isFinite")?;
        self.set_intrinsic_data_property(number, is_finite_atom, is_finite, true)?;
        let is_integer = allocate(self, NativeFunction::NumberIsInteger)?;
        self.realm.number_is_integer = Some(is_integer);
        let is_integer_atom = self.intern_intrinsic_name(b"isInteger")?;
        self.set_intrinsic_data_property(number, is_integer_atom, is_integer, true)?;
        let is_safe_integer = allocate(self, NativeFunction::NumberIsSafeInteger)?;
        self.realm.number_is_safe_integer = Some(is_safe_integer);
        let is_safe_integer_atom = self.intern_intrinsic_name(b"isSafeInteger")?;
        self.set_intrinsic_data_property(number, is_safe_integer_atom, is_safe_integer, true)?;
        for (name, value) in [
            (b"EPSILON".as_slice(), Value::from_f64(f64::EPSILON)),
            (b"MAX_VALUE".as_slice(), Value::from_f64(f64::MAX)),
            (b"MIN_VALUE".as_slice(), Value::from_f64(f64::from_bits(1))),
            (
                b"MAX_SAFE_INTEGER".as_slice(),
                Value::from_f64(MAX_SAFE_INTEGER as f64),
            ),
            (
                b"MIN_SAFE_INTEGER".as_slice(),
                Value::from_f64(-(MAX_SAFE_INTEGER as f64)),
            ),
            (b"NaN".as_slice(), Value::from_f64(f64::NAN)),
            (
                b"POSITIVE_INFINITY".as_slice(),
                Value::from_f64(f64::INFINITY),
            ),
            (
                b"NEGATIVE_INFINITY".as_slice(),
                Value::from_f64(f64::NEG_INFINITY),
            ),
        ] {
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_constant_property(number, atom, value)?;
        }
        self.realm.boolean_constructor = Some(allocate(self, NativeFunction::BooleanConstructor)?);
        Ok(())
    }

    /// Builds the callable prototype chain before constructors depend on `%Function.prototype%`.
    fn initialize_function_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let call_atom = self.intern_intrinsic_name(b"call")?;
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Function prototype");
        let call = self.allocate_native_function(
            NativeFunction::FunctionPrototypeCall,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: object_prototype,
            },
        )?;
        self.realm.function_prototype_call = Some(call);
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                call_atom,
                PropertyAttributes::data(true, false, true),
            )
            .map_err(ExecutionError::Shape)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage {
                    slots: Box::new([call]),
                },
                AllocationSpace::Old,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let function_prototype = self.allocate_native_function(
            NativeFunction::FunctionPrototype,
            OrdinaryObject {
                shape,
                extensible: true,
                storage: Some(storage),
                prototype: object_prototype,
            },
        )?;
        self.realm.function_prototype = Some(function_prototype);
        self.set_function_internal_prototype(call, function_prototype)?;
        let bind = self.allocate_native_function(
            NativeFunction::FunctionPrototypeBind,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.function_prototype_bind = Some(bind);
        let bind_atom = self.intern_intrinsic_name(b"bind")?;
        self.set_intrinsic_data_property(function_prototype, bind_atom, bind, true)?;
        let constructor = self.allocate_native_function(
            NativeFunction::FunctionConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.function_constructor = Some(constructor);
        self.set_function_prototype(constructor, function_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(function_prototype, constructor_atom, constructor, true)
    }

    /// Interns every mandatory global name before reserving the dense atom-indexed binding table.
    fn intern_realm_intrinsic_atoms(&mut self) -> Result<RealmIntrinsicAtoms, ExecutionError> {
        Ok(RealmIntrinsicAtoms {
            undefined: self.intern_intrinsic_name(b"undefined")?,
            nan: self.intern_intrinsic_name(b"NaN")?,
            infinity: self.intern_intrinsic_name(b"Infinity")?,
            errors: [
                self.intern_intrinsic_name(b"Error")?,
                self.intern_intrinsic_name(b"ReferenceError")?,
                self.intern_intrinsic_name(b"SyntaxError")?,
                self.intern_intrinsic_name(b"TypeError")?,
                self.intern_intrinsic_name(b"RangeError")?,
            ],
            array: self.intern_intrinsic_name(b"Array")?,
            object: self.intern_intrinsic_name(b"Object")?,
            string: self.intern_intrinsic_name(b"String")?,
            symbol: self.intern_intrinsic_name(b"Symbol")?,
            number: self.intern_intrinsic_name(b"Number")?,
            boolean: self.intern_intrinsic_name(b"Boolean")?,
            function: self.intern_intrinsic_name(b"Function")?,
            math: self.intern_intrinsic_name(b"Math")?,
        })
    }

    /// Creates the base Error pair first, then roots each subclass pair before property allocation.
    fn initialize_error_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before Error intrinsics");
        let constructor_atom = self.constructor_atom()?;
        for kind in NativeErrorKind::ALL {
            let parent = if kind == NativeErrorKind::Error {
                self.realm
                    .object_prototype
                    .expect("Object prototype initializes before Error prototypes")
            } else {
                self.realm
                    .error_intrinsics
                    .get(NativeErrorKind::Error)
                    .prototype
                    .expect("Error.prototype initializes before subclasses")
            };
            let prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: parent,
            })?;
            self.realm.error_intrinsics.get_mut(kind).prototype = Some(prototype);
            let constructor = self.allocate_native_function(
                NativeFunction::ErrorConstructor(kind),
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            self.realm.error_intrinsics.get_mut(kind).constructor = Some(constructor);
            self.set_function_prototype(constructor, prototype)?;
            self.set_intrinsic_data_property(prototype, constructor_atom, constructor, true)?;
        }
        Ok(())
    }

    /// Builds the ordinary Array constructor/prototype pair and its first indexed method.
    fn initialize_array_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before Array intrinsics");
        let prototype = self.allocate_array_object(
            self.realm
                .object_prototype
                .expect("Object prototype initializes before Array prototype"),
            AllocationSpace::Old,
        )?;
        self.realm.array_prototype = Some(prototype);
        let constructor = self.allocate_native_function(
            NativeFunction::ArrayConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_constructor = Some(constructor);
        self.set_function_prototype(constructor, prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(prototype, constructor_atom, constructor, true)?;
        let is_array = self.allocate_native_function(
            NativeFunction::ArrayIsArray,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_is_array = Some(is_array);
        let is_array_atom = self.intern_intrinsic_name(b"isArray")?;
        self.set_intrinsic_data_property(constructor, is_array_atom, is_array, true)?;
        let length_atom = self.intern_intrinsic_name(b"length")?;
        self.set_intrinsic_data_property(prototype, length_atom, Value::from_i32(0), false)?;
        let concat = self.allocate_native_function(
            NativeFunction::ArrayConcat,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_concat = Some(concat);
        let concat_atom = self.intern_intrinsic_name(b"concat")?;
        self.set_intrinsic_data_property(prototype, concat_atom, concat, true)?;
        let push = self.allocate_native_function(
            NativeFunction::ArrayPush,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_push = Some(push);
        let push_atom = self.intern_intrinsic_name(b"push")?;
        self.set_intrinsic_data_property(prototype, push_atom, push, true)?;
        let join = self.allocate_native_function(
            NativeFunction::ArrayJoin,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_join = Some(join);
        let join_atom = self.intern_intrinsic_name(b"join")?;
        self.set_intrinsic_data_property(prototype, join_atom, join, true)?;
        let at = self.allocate_native_function(
            NativeFunction::ArrayAt,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_at = Some(at);
        let at_atom = self.intern_intrinsic_name(b"at")?;
        self.set_intrinsic_data_property(prototype, at_atom, at, true)?;
        let index_of = self.allocate_native_function(
            NativeFunction::ArrayIndexOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_index_of = Some(index_of);
        let index_of_atom = self.intern_intrinsic_name(b"indexOf")?;
        self.set_intrinsic_data_property(prototype, index_of_atom, index_of, true)?;
        let includes = self.allocate_native_function(
            NativeFunction::ArrayIncludes,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_includes = Some(includes);
        let includes_atom = self.intern_intrinsic_name(b"includes")?;
        self.set_intrinsic_data_property(prototype, includes_atom, includes, true)?;
        let pop = self.allocate_native_function(
            NativeFunction::ArrayPop,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_pop = Some(pop);
        let pop_atom = self.intern_intrinsic_name(b"pop")?;
        self.set_intrinsic_data_property(prototype, pop_atom, pop, true)?;
        let slice = self.allocate_native_function(
            NativeFunction::ArraySlice,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_slice = Some(slice);
        let slice_atom = self.intern_intrinsic_name(b"slice")?;
        self.set_intrinsic_data_property(prototype, slice_atom, slice, true)?;
        let shift = self.allocate_native_function(
            NativeFunction::ArrayShift,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_shift = Some(shift);
        let shift_atom = self.intern_intrinsic_name(b"shift")?;
        self.set_intrinsic_data_property(prototype, shift_atom, shift, true)?;
        let unshift = self.allocate_native_function(
            NativeFunction::ArrayUnshift,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_unshift = Some(unshift);
        let unshift_atom = self.intern_intrinsic_name(b"unshift")?;
        self.set_intrinsic_data_property(prototype, unshift_atom, unshift, true)?;
        let reverse = self.allocate_native_function(
            NativeFunction::ArrayReverse,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_reverse = Some(reverse);
        let reverse_atom = self.intern_intrinsic_name(b"reverse")?;
        self.set_intrinsic_data_property(prototype, reverse_atom, reverse, true)?;
        let fill = self.allocate_native_function(
            NativeFunction::ArrayFill,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_fill = Some(fill);
        let fill_atom = self.intern_intrinsic_name(b"fill")?;
        self.set_intrinsic_data_property(prototype, fill_atom, fill, true)?;
        let last_index_of = self.allocate_native_function(
            NativeFunction::ArrayLastIndexOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_last_index_of = Some(last_index_of);
        let last_index_of_atom = self.intern_intrinsic_name(b"lastIndexOf")?;
        self.set_intrinsic_data_property(prototype, last_index_of_atom, last_index_of, true)?;
        let copy_within = self.allocate_native_function(
            NativeFunction::ArrayCopyWithin,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_copy_within = Some(copy_within);
        let copy_within_atom = self.intern_intrinsic_name(b"copyWithin")?;
        self.set_intrinsic_data_property(prototype, copy_within_atom, copy_within, true)?;
        let flat = self.allocate_native_function(
            NativeFunction::ArrayFlat,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_flat = Some(flat);
        let flat_atom = self.intern_intrinsic_name(b"flat")?;
        self.set_intrinsic_data_property(prototype, flat_atom, flat, true)?;
        let sort = self.allocate_native_function(
            NativeFunction::ArraySort,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_sort = Some(sort);
        let sort_atom = self.intern_intrinsic_name(b"sort")?;
        self.set_intrinsic_data_property(prototype, sort_atom, sort, true)?;
        let to_string = self.allocate_native_function(
            NativeFunction::ArrayToString,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_to_string = Some(to_string);
        let to_string_atom = self.intern_intrinsic_name(b"toString")?;
        self.set_intrinsic_data_property(prototype, to_string_atom, to_string, true)
    }

    /// Builds the non-constructor Math namespace and its first numeric intrinsic.
    fn initialize_math_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let object = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: self
                .realm
                .object_prototype
                .expect("Object prototype initializes before Math"),
        })?;
        self.realm.math_object = Some(object);
        let pow = self.allocate_native_function(
            NativeFunction::MathPow,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: self
                    .realm
                    .function_prototype
                    .expect("Function prototype initializes before Math methods"),
            },
        )?;
        self.realm.math_pow = Some(pow);
        let pow_atom = self.intern_intrinsic_name(b"pow")?;
        self.set_intrinsic_data_property(object, pow_atom, pow, true)
    }

    /// Publishes all mandatory names without charging the host quota for user-created globals.
    fn publish_realm_intrinsic_bindings(
        &mut self,
        atoms: RealmIntrinsicAtoms,
    ) -> Result<(), ExecutionError> {
        self.realm.reserve_intrinsics(
            RealmIntrinsicAtoms::BINDING_COUNT,
            self.atoms.stats().entries,
        )?;
        self.realm.publish_intrinsic(
            atoms.undefined,
            Value::from_immediate(Immediate::Undefined),
            false,
        )?;
        self.realm
            .publish_intrinsic(atoms.nan, Value::from_f64(f64::NAN), false)?;
        self.realm
            .publish_intrinsic(atoms.infinity, Value::from_f64(f64::INFINITY), false)?;
        for kind in NativeErrorKind::ALL {
            let constructor = self
                .realm
                .error_intrinsics
                .get(kind)
                .constructor
                .expect("Error constructor initializes before global publication");
            self.realm
                .publish_intrinsic(atoms.error(kind), constructor, true)?;
        }
        self.realm.publish_intrinsic(
            atoms.array,
            self.realm
                .array_constructor
                .expect("Array initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.object,
            self.realm
                .object_constructor
                .expect("Object initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.string,
            self.realm
                .string_constructor
                .expect("String initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.symbol,
            self.realm
                .symbol_constructor
                .expect("Symbol initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.number,
            self.realm
                .number_constructor
                .expect("Number initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.boolean,
            self.realm
                .boolean_constructor
                .expect("Boolean initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.function,
            self.realm
                .function_constructor
                .expect("Function initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.math,
            self.realm
                .math_object
                .expect("Math initializes before global publication"),
            true,
        )?;
        Ok(())
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

    /// Creates an Array-shaped ordinary object from one native call/construct argument window.
    fn create_array_from_site(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let count = usize::try_from(site.argument_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            );
        }
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before Array construction");
        let array = self.create_array_object_with_prototype(prototype)?;
        self.write(site.caller_base, site.destination, array)?;
        let length_atom = self.intern_intrinsic_name(b"length")?;
        if arguments.len() == 1
            && let Some(length) = arguments[0].as_i32()
            && length >= 0
        {
            self.set_own_data_property(array, length_atom, arguments[0])?;
            return Ok(array);
        }
        for (index, value) in arguments.into_iter().enumerate() {
            let index = i32::try_from(index)
                .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
            let key = self.property_key_atom(Value::from_i32(index))?;
            self.set_own_data_property(array, key, value)?;
        }
        let length = Value::from_i32(
            i32::try_from(count)
                .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?,
        );
        self.set_own_data_property(array, length_atom, length)?;
        Ok(array)
    }

    /// Implements the ordinary Object constructor for object values and primitive fallback values.
    fn create_object_from_site(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        if let Some(value) = self.call_argument(site, 0)?
            && self.is_object_value(value)
        {
            return Ok(value);
        }
        let object = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, object)?;
        Ok(object)
    }

    /// Executes one primitive constructor using the exact call argument window.
    fn primitive_constructor_value(
        &mut self,
        native: NativeFunction,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        match native {
            NativeFunction::StringConstructor => self.primitive_string_value(argument),
            NativeFunction::SymbolConstructor => self.allocate_symbol(
                argument.filter(|value| value.as_immediate() != Some(Immediate::Undefined)),
            ),
            NativeFunction::NumberConstructor => {
                let argument = argument.unwrap_or(Value::from_i32(0));
                self.convert_to_number(argument)
            }
            NativeFunction::BooleanConstructor => {
                let argument = argument.unwrap_or(Value::from_immediate(Immediate::Undefined));
                Ok(Value::from_immediate(if self.is_truthy_value(argument)? {
                    Immediate::True
                } else {
                    Immediate::False
                }))
            }
            _ => Err(ExecutionError::NonCallable(Value::from_immediate(
                Immediate::Undefined,
            ))),
        }
    }

    /// Converts one already-primitive String constructor argument into its canonical string value.
    fn primitive_string_value(&mut self, argument: Option<Value>) -> Result<Value, ExecutionError> {
        let Some(argument) = argument else {
            return self.allocate_runtime_string(
                JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
            );
        };
        if self.is_string_value(argument) {
            return Ok(argument);
        }
        let mut units = Vec::new();
        self.append_primitive_string_units(argument, &mut units)?;
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Allocates one unique Symbol primitive while retaining its optional description as a GC edge.
    fn allocate_symbol(&mut self, description: Option<Value>) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.symbol,
                0,
                0,
                SymbolValue { description },
                AllocationSpace::Young,
                roots,
            )
            .map(|symbol| Value::from_heap_ref(symbol.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Implements the shared thisNumberValue brand check for Number prototype methods.
    fn this_number_value(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        if numeric_value(receiver).is_some() {
            return Ok(receiver);
        }
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let number = self
            .heap
            .checked_reference(raw, self.types.number_object)
            .map_err(|_| ExecutionError::NotObject(receiver))?;
        self.heap.with_running_scope(|scope| {
            let number = scope.root(number).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(number, self.types.number_object)
                    .map(|number| number.number_data)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Applies the primitive subset of ToIntegerOrInfinity with an undefined default.
    fn integer_or_infinity_argument(
        &mut self,
        argument: Option<Value>,
        default: f64,
    ) -> Result<f64, ExecutionError> {
        let Some(argument) = argument else {
            return Ok(default);
        };
        if argument.as_immediate() == Some(Immediate::Undefined) {
            return Ok(default);
        }
        let converted = self.convert_to_number(argument)?;
        let number = numeric_value(converted)
            .ok_or(ExecutionError::UnsupportedNumberConversion(argument))?;
        if number.is_nan() || number == 0.0 {
            return Ok(0.0);
        }
        Ok(number.trunc())
    }

    /// Implements Number.prototype.toFixed with the pinned ECMAScript decimal formatter.
    fn number_to_fixed(
        &mut self,
        receiver: Value,
        fraction_digits: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let fraction_digits = self.integer_or_infinity_argument(fraction_digits, 0.0)?;
        if !(0.0..=100.0).contains(&fraction_digits) {
            return Err(ExecutionError::InvalidNumberPrecision(Value::from_f64(
                fraction_digits,
            )));
        }
        let number = numeric_value(number).expect("thisNumberValue always returns a number");
        let mut buffer = ryu_js::Buffer::new();
        let formatted = buffer.format_to_fixed(number, fraction_digits as u8);
        self.allocate_runtime_string(
            JsString::try_from_latin1(formatted.as_bytes())
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements Number.prototype.toExponential with exact binary-rational rounding.
    fn number_to_exponential(
        &mut self,
        receiver: Value,
        fraction_digits: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let fraction_digits = match fraction_digits {
            Some(value) if value.as_immediate() != Some(Immediate::Undefined) => {
                Some(self.integer_or_infinity_argument(Some(value), 0.0)?)
            }
            _ => None,
        };
        let number = numeric_value(number).expect("thisNumberValue always returns a number");
        if number.is_finite()
            && fraction_digits.is_some_and(|digits| !(0.0..=100.0).contains(&digits))
        {
            return Err(ExecutionError::InvalidNumberPrecision(Value::from_f64(
                fraction_digits.unwrap_or_default(),
            )));
        }
        let fraction_digits = fraction_digits.map(|digits| digits as u8);
        let mut buffer = [0; tuning::numbers::EXPONENTIAL_FORMAT_BUFFER_SIZE];
        let formatted =
            number::format_exponential(number, fraction_digits, &mut buffer).map_err(|error| {
                match error {
                    number::NumberFormatError::BufferExhausted => {
                        ExecutionError::NumberFormatBufferExhausted
                    }
                    number::NumberFormatError::InvalidDigit => {
                        ExecutionError::NumberFormatInvalidDigit
                    }
                }
            })?;
        self.allocate_runtime_string(
            JsString::try_from_latin1(formatted).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements Number.prototype.toPrecision with shared exact significant-digit rounding.
    fn number_to_precision(
        &mut self,
        receiver: Value,
        precision: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let Some(precision) =
            precision.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            let mut units = Vec::new();
            self.append_primitive_string_units(number, &mut units)?;
            return self.allocate_runtime_string(
                JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
            );
        };
        let precision = self.integer_or_infinity_argument(Some(precision), 0.0)?;
        let number = numeric_value(number).expect("thisNumberValue always returns a number");
        if number.is_finite() && !(1.0..=100.0).contains(&precision) {
            return Err(ExecutionError::InvalidNumberPrecision(Value::from_f64(
                precision,
            )));
        }
        let mut buffer = [0; tuning::numbers::EXPONENTIAL_FORMAT_BUFFER_SIZE];
        let formatted =
            number::format_precision(number, precision as u8, &mut buffer).map_err(|error| {
                match error {
                    number::NumberFormatError::BufferExhausted => {
                        ExecutionError::NumberFormatBufferExhausted
                    }
                    number::NumberFormatError::InvalidDigit => {
                        ExecutionError::NumberFormatInvalidDigit
                    }
                }
            })?;
        self.allocate_runtime_string(
            JsString::try_from_latin1(formatted).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Starts one conversion consumer, suspending only when its argument requires a JS callback.
    fn dispatch_conversion_native(
        &mut self,
        native: NativeFunction,
        site: &CallSite,
        construct: bool,
    ) -> Result<(), ExecutionError> {
        let consumer = if construct {
            ConversionConsumer::NativeConstruct(native)
        } else {
            ConversionConsumer::NativeCall(native)
        };
        let argument = self.call_argument(site, 0)?;
        if let Some(object) = argument
            && self.is_object_value(object)
        {
            let (receiver, stage) = match native {
                NativeFunction::StringConstructor => (
                    Value::from_immediate(Immediate::Undefined),
                    ToPrimitiveStage::ToString,
                ),
                NativeFunction::NumberToExponential
                | NativeFunction::NumberToFixed
                | NativeFunction::NumberToPrecision
                | NativeFunction::NumberToString => (
                    self.this_number_value(site.this_value)?,
                    ToPrimitiveStage::ValueOf,
                ),
                NativeFunction::NumberConstructor => (
                    if construct {
                        site.new_target
                    } else {
                        Value::from_immediate(Immediate::Undefined)
                    },
                    ToPrimitiveStage::ValueOf,
                ),
                _ => unreachable!("only conversion consumers enter this dispatch path"),
            };
            let continuation = NativeContinuation {
                site: NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                consumer,
                receiver,
                object,
                stage,
            };
            return self.advance_native_conversion(continuation, None);
        }
        let receiver = if construct {
            site.new_target
        } else {
            site.this_value
        };
        let value = self.finish_conversion_consumer(consumer, receiver, argument)?;
        self.write(site.caller_base, site.destination, value)
    }

    /// Starts a cold object conversion while tracing one optional pending operand.
    #[cold]
    #[inline(never)]
    fn dispatch_object_numeric_conversion(
        &mut self,
        consumer: ConversionConsumer,
        caller_base: u32,
        destination: u32,
        pending: Value,
        object: Value,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        debug_assert!(self.is_object_value(object));
        debug_assert!(consumer.is_numeric_operation());
        self.advance_native_conversion(
            NativeContinuation {
                site: NativeContinuationSite {
                    caller_base,
                    destination,
                    call_site,
                },
                consumer,
                receiver: pending,
                object,
                stage: ToPrimitiveStage::ValueOf,
            },
            None,
        )
    }

    /// Advances ordinary ToPrimitive without recursively entering the interpreter.
    fn advance_native_conversion(
        &mut self,
        mut continuation: NativeContinuation,
        mut returned: Option<Value>,
    ) -> Result<(), ExecutionError> {
        loop {
            if let Some(value) = returned.take() {
                if !self.is_object_value(value) {
                    if let ConversionConsumer::BinaryLeft(opcode) = continuation.consumer {
                        let left = self.convert_to_number(value)?;
                        let right = continuation.receiver;
                        if self.is_object_value(right) {
                            continuation.consumer = ConversionConsumer::BinaryRight(opcode);
                            continuation.receiver = left;
                            continuation.object = right;
                            continuation.stage = ToPrimitiveStage::ValueOf;
                            continue;
                        }
                        let right = self.convert_to_number(right)?;
                        let result = numeric_binary_operation(opcode, left, right);
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if let ConversionConsumer::BinaryRight(opcode) = continuation.consumer {
                        let right = self.convert_to_number(value)?;
                        let result = numeric_binary_operation(opcode, continuation.receiver, right);
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    let result = self.finish_conversion_consumer(
                        continuation.consumer,
                        continuation.receiver,
                        Some(value),
                    )?;
                    return self.write(
                        continuation.site.caller_base,
                        continuation.site.destination,
                        result,
                    );
                }
                let Some(stage) =
                    next_to_primitive_stage(continuation.consumer, continuation.stage)
                else {
                    return Err(ExecutionError::NotObject(continuation.object));
                };
                continuation.stage = stage;
            }
            let name = match continuation.stage {
                ToPrimitiveStage::ValueOf => b"valueOf".as_slice(),
                ToPrimitiveStage::ToString => b"toString".as_slice(),
            };
            let atom = self.intern_intrinsic_name(name)?;
            let method = self.get_data_property(continuation.object, atom)?;
            let Some(method) =
                method.filter(|method| self.resolve_function_object(*method).is_ok())
            else {
                let Some(stage) =
                    next_to_primitive_stage(continuation.consumer, continuation.stage)
                else {
                    return Err(ExecutionError::NotObject(continuation.object));
                };
                continuation.stage = stage;
                continue;
            };
            self.fiber
                .completions
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
            self.fiber
                .completions
                .push(Completion::Native(continuation));
            let frame_depth = self.fiber.frames.len();
            let call_result = self.call(CallSite {
                caller_base: continuation.site.caller_base,
                destination: continuation.site.destination,
                callee: method,
                argument_base: 0,
                argument_prefix: None,
                argument_prefix_offset: 0,
                argument_prefix_count: 0,
                argument_count: 0,
                this_value: continuation.object,
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: continuation.site.call_site,
            });
            if let Err(error) = call_result {
                self.pop_native_continuation()?;
                return Err(error);
            }
            if self.fiber.frames.len() != frame_depth {
                let frame = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("a suspended callback publishes its callee frame");
                frame.return_register = None;
                frame.return_continuation = true;
                return Ok(());
            }
            continuation = self.pop_native_continuation()?;
            returned =
                Some(self.read(continuation.site.caller_base, continuation.site.destination)?);
        }
    }

    /// Removes the exact native sentinel published before a callback call attempt.
    #[inline]
    fn pop_native_continuation(&mut self) -> Result<NativeContinuation, ExecutionError> {
        match self.fiber.completions.pop() {
            Some(Completion::Native(continuation)) => Ok(continuation),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Completes one native consumer after its optional argument has become the required primitive.
    fn finish_conversion_consumer(
        &mut self,
        consumer: ConversionConsumer,
        receiver: Value,
        argument: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let Some(native) = consumer.native() else {
            let Some(argument) = argument else {
                return Err(ExecutionError::MissingNativeContinuation);
            };
            return Ok(match consumer {
                ConversionConsumer::ToNumber => self.convert_to_number(argument)?,
                ConversionConsumer::Negate => numeric_negate(self.convert_to_number(argument)?),
                ConversionConsumer::BitwiseNot => {
                    numeric_bitwise_not(self.convert_to_number(argument)?)
                }
                ConversionConsumer::BinaryLeft(_) | ConversionConsumer::BinaryRight(_) => {
                    unreachable!("binary consumers finish inside the conversion state machine")
                }
                ConversionConsumer::NativeCall(_) | ConversionConsumer::NativeConstruct(_) => {
                    unreachable!("native conversion consumers always carry a native function")
                }
            });
        };
        match native {
            NativeFunction::StringConstructor => self.primitive_string_value(argument),
            NativeFunction::NumberConstructor => {
                let number = self.convert_to_number(argument.unwrap_or(Value::from_i32(0)))?;
                if matches!(consumer, ConversionConsumer::NativeConstruct(_)) {
                    self.box_number_from_constructor(number, receiver)
                } else {
                    Ok(number)
                }
            }
            NativeFunction::NumberToExponential => self.number_to_exponential(receiver, argument),
            NativeFunction::NumberToFixed => self.number_to_fixed(receiver, argument),
            NativeFunction::NumberToPrecision => self.number_to_precision(receiver, argument),
            NativeFunction::NumberToString => self.number_to_string(receiver, argument),
            _ => unreachable!("only conversion consumers create this continuation"),
        }
    }

    /// Allocates a Number wrapper only after constructor argument conversion has completed.
    fn box_number_from_constructor(
        &mut self,
        number: Value,
        new_target: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or_else(|| {
                self.realm
                    .number_prototype
                    .expect("Number prototype initializes before construction")
            });
        self.allocate_number_object(number, prototype, AllocationSpace::Young)
    }

    /// Implements Number::toString for decimal and shortest round-trip radix representations.
    fn number_to_string(
        &mut self,
        receiver: Value,
        radix: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let radix_number = if let Some(radix) = radix
            && radix.as_immediate() != Some(Immediate::Undefined)
        {
            let converted = self.convert_to_number(radix)?;
            let radix_number = numeric_value(converted)
                .ok_or(ExecutionError::UnsupportedNumberConversion(radix))?;
            let integer = if radix_number.is_nan() {
                0.0
            } else {
                radix_number.trunc()
            };
            if !(2.0..=36.0).contains(&integer) {
                return Err(ExecutionError::InvalidNumberRadix(radix));
            }
            integer as u8
        } else {
            10
        };
        let numeric = numeric_value(number).expect("thisNumberValue always returns a number");
        if radix_number != 10 && numeric.is_finite() && numeric != 0.0 {
            let mut buffer = [0; tuning::numbers::RADIX_FORMAT_BUFFER_SIZE];
            let bytes =
                number::format_radix(numeric, radix_number, &mut buffer).map_err(|error| {
                    match error {
                        number::NumberFormatError::BufferExhausted => {
                            ExecutionError::NumberFormatBufferExhausted
                        }
                        number::NumberFormatError::InvalidDigit => {
                            ExecutionError::NumberFormatInvalidDigit
                        }
                    }
                })?;
            return self.allocate_runtime_string(
                JsString::try_from_latin1(bytes).map_err(ExecutionError::PropertyKeyString)?,
            );
        }
        let mut units = Vec::new();
        self.append_primitive_string_units(number, &mut units)?;
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements the ordinary tag-producing subset of Object.prototype.toString.
    fn object_to_string(&mut self, value: Value) -> Result<Value, ExecutionError> {
        let tag = if let Some(immediate) = value.as_immediate() {
            match immediate {
                Immediate::Undefined => "[object Undefined]",
                Immediate::Null => "[object Null]",
                Immediate::True | Immediate::False => "[object Boolean]",
                Immediate::Hole | Immediate::Uninitialized => "[object Object]",
            }
        } else if value.as_i32().is_some()
            || value.as_f64().is_some()
            || value.as_heap_ref().is_some_and(|raw| {
                self.heap
                    .checked_reference(raw, self.types.number_object)
                    .is_ok()
            })
        {
            "[object Number]"
        } else if let Some(raw) = value.as_heap_ref()
            && self.heap.checked_reference(raw, self.types.string).is_ok()
        {
            "[object String]"
        } else if let Some(raw) = value.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.function)
                .is_ok()
        {
            "[object Function]"
        } else if self.is_array_value(value)? {
            "[object Array]"
        } else {
            "[object Object]"
        };
        self.allocate_runtime_string(
            JsString::try_from_latin1(tag.as_bytes()).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Appends one array-like source while preserving holes as length-only positions.
    fn append_array_source(
        &mut self,
        destination: Value,
        source: Value,
        next_index: &mut i32,
    ) -> Result<(), ExecutionError> {
        if let Some(raw) = source.as_heap_ref()
            && let Ok(reference) = self.heap.checked_reference(raw, self.types.string)
        {
            let units = self.heap.with_running_scope(|scope| {
                let root = scope.root(reference).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let string = no_gc
                        .borrow(root, self.types.string)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    Ok::<Vec<u16>, ExecutionError>(match string.as_view() {
                        JsStringView::Latin1(bytes) => {
                            bytes.iter().map(|&byte| u16::from(byte)).collect()
                        }
                        JsStringView::Utf16(units) => units.to_vec(),
                    })
                })
            })?;
            for unit in units {
                let character = self.allocate_runtime_string(
                    JsString::try_from_utf16(&[unit]).map_err(ExecutionError::PropertyKeyString)?,
                )?;
                let key = self.property_key_atom(Value::from_i32(*next_index))?;
                self.set_own_data_property(destination, key, character)?;
                *next_index = next_index
                    .checked_add(1)
                    .ok_or(ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
            }
            return Ok(());
        }
        if !self.is_array_value(source)? {
            let key = self.property_key_atom(Value::from_i32(*next_index))?;
            self.set_own_data_property(destination, key, source)?;
            *next_index = next_index
                .checked_add(1)
                .ok_or(ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
            return Ok(());
        }
        let length_atom = self.intern_intrinsic_name(b"length")?;
        let length_value = self
            .get_data_property(source, length_atom)?
            .expect("every Tachyon Array has a length property");
        let Some(length) = length_value.as_i32() else {
            return Err(ExecutionError::UnsupportedNumberConversion(length_value));
        };
        if length < 0 {
            return Err(ExecutionError::UnsupportedNumberConversion(length_value));
        }
        for index in 0..length {
            let key = self.property_key_atom(Value::from_i32(index))?;
            if let Some(value) = self.get_data_property(source, key)? {
                let destination_key = self.property_key_atom(Value::from_i32(*next_index))?;
                self.set_own_data_property(destination, destination_key, value)?;
            }
            *next_index = next_index
                .checked_add(1)
                .ok_or(ExecutionError::RegisterWindowTooLarge(length as u32))?;
        }
        Ok(())
    }

    /// Implements the non-Proxy IsArray branch through the unforgeable GC payload type.
    fn is_array_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        Ok(value
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.array).is_ok()))
    }

    /// Returns the current ordinary prototype and applies the nullish TypeError boundary.
    fn object_prototype_of(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if matches!(
            value.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(value));
        }
        if self.is_object_value(value) {
            return self
                .object_snapshot(value)
                .map(|(_, object)| object.prototype);
        }
        Ok(self
            .realm
            .object_prototype
            .expect("Object prototype initializes before primitive boxing"))
    }

    /// Walks one ordinary prototype chain without invoking user code or allocating.
    fn is_prototype_of(
        &mut self,
        prototype: Value,
        value: Option<Value>,
    ) -> Result<bool, ExecutionError> {
        let Some(value) = value else {
            return Ok(false);
        };
        if !self.is_object_value(prototype) || !self.is_object_value(value) {
            return Ok(false);
        }
        let (_, mut snapshot) = self.object_snapshot(value)?;
        loop {
            if snapshot.prototype == prototype {
                return Ok(true);
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(false);
            }
            let (_, next) = self.object_snapshot(snapshot.prototype)?;
            snapshot = next;
        }
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

    /// Implements Array.prototype.push through the generic array-like Set contract.
    fn array_push(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let argument_count = u64::from(site.argument_count);
        let new_length = length
            .checked_add(argument_count)
            .filter(|length| *length <= MAX_SAFE_INTEGER)
            .ok_or(ExecutionError::ArrayLengthOverflow)?;
        for index in 0..site.argument_count {
            let value = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            let key = self.safe_integer_property_atom(
                length
                    .checked_add(u64::from(index))
                    .ok_or(ExecutionError::ArrayLengthOverflow)?,
            )?;
            self.set_own_data_property(site.this_value, key, value)?;
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(site.this_value, length_atom, safe_integer_value(new_length))?;
        Ok(safe_integer_value(new_length))
    }

    fn array_join(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let separator = self.call_argument(site, 0)?;
        self.join_array_like(site.this_value, separator)
    }

    /// Joins one generic array-like receiver while retaining primitive conversion order.
    fn join_array_like(
        &mut self,
        receiver: Value,
        separator: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(receiver)?;
        let mut separator_units = Vec::new();
        if separator.is_none_or(|value| value.as_immediate() == Some(Immediate::Undefined)) {
            separator_units
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            separator_units.push(u16::from(b','));
        } else if let Some(separator) = separator {
            self.append_primitive_string_units(separator, &mut separator_units)?;
        }
        let per_element =
            tuning::arrays::JOIN_INITIAL_UNITS_PER_ELEMENT.saturating_add(separator_units.len());
        let estimated = usize::try_from(length)
            .unwrap_or(usize::MAX)
            .saturating_mul(per_element)
            .min(tuning::arrays::JOIN_MAX_INITIAL_UNITS);
        let mut output = Vec::new();
        output
            .try_reserve_exact(estimated)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..length {
            if index != 0 {
                output
                    .try_reserve(separator_units.len())
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                output.extend_from_slice(&separator_units);
            }
            let key = self.safe_integer_property_atom(index)?;
            let value = self
                .get_data_property(receiver, key)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            if value == receiver
                || matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                )
            {
                continue;
            }
            self.append_primitive_string_units(value, &mut output)?;
        }
        let string =
            JsString::try_from_utf16(&output).map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(string)
    }

    /// Applies the currently supported ToLength boundary to one object length property.
    fn length_of_array_like(&mut self, receiver: Value) -> Result<u64, ExecutionError> {
        if !self.is_object_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let length_atom = self.length_atom()?;
        let value = self
            .get_data_property(receiver, length_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let number = self.convert_to_number(value)?;
        let number =
            numeric_value(number).ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
            return Ok(MAX_SAFE_INTEGER);
        }
        Ok(number.floor() as u64)
    }

    #[inline(always)]
    fn safe_integer_property_atom(&mut self, index: u64) -> Result<AtomId, ExecutionError> {
        debug_assert!(index <= MAX_SAFE_INTEGER);
        if let Ok(integer) = i32::try_from(index) {
            return self.property_key_atom(Value::from_i32(integer));
        }
        self.property_key_atom(Value::from_f64(index as f64))
    }

    /// Implements the basic Array.prototype.concat flattening contract for ordinary arrays.
    fn array_concat(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        let mut next_index = 0_i32;
        self.append_array_source(result, site.this_value, &mut next_index)?;
        for index in 0..site.argument_count {
            let argument = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            self.append_array_source(result, argument, &mut next_index)?;
        }
        let length_atom = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(result, length_atom, Value::from_i32(next_index))?;
        Ok(result)
    }

    /// Implements `Array.prototype.at` for the supported generic array-like receiver.
    fn array_at(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        if length == 0 {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        let index_value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let number = numeric_value(self.convert_to_number(index_value)?).unwrap_or(f64::NAN);
        if number.is_nan() {
            return self.array_element_or_undefined(site.this_value, 0);
        }
        let index = if number < 0.0 {
            length as f64 + number.ceil()
        } else {
            number.floor()
        };
        if !(0.0..(length as f64)).contains(&index) {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        self.array_element_or_undefined(site.this_value, index as u64)
    }

    /// Implements `indexOf` and `includes` without allocating an iterator or callback closure.
    fn array_search(&mut self, site: &CallSite, includes: bool) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        if length == 0 {
            return Ok(if includes {
                Value::from_immediate(Immediate::False)
            } else {
                Value::from_i32(-1)
            });
        }
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let start_value = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let start_number = numeric_value(self.convert_to_number(start_value)?).unwrap_or(f64::NAN);
        let start = if start_number.is_nan() {
            0
        } else if start_number < 0.0 {
            length.saturating_sub((-start_number).ceil() as u64)
        } else {
            start_number.floor() as u64
        };
        for index in start..length {
            let key = self.safe_integer_property_atom(index)?;
            let Some(value) = self.get_data_property(site.this_value, key)? else {
                if includes && search.as_immediate() == Some(Immediate::Undefined) {
                    return Ok(Value::from_immediate(Immediate::True));
                }
                continue;
            };
            let equal = if includes {
                self.same_value_zero(value, search)?
            } else {
                self.strict_equal_values(value, search)?
            };
            if equal {
                return Ok(if includes {
                    Value::from_immediate(Immediate::True)
                } else {
                    safe_integer_value(index)
                });
            }
        }
        Ok(if includes {
            Value::from_immediate(Immediate::False)
        } else {
            Value::from_i32(-1)
        })
    }

    /// Implements `Array.prototype.pop` through the generic array-like property contract.
    fn array_pop(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        if length == 0 {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        let index = length - 1;
        let key = self.safe_integer_property_atom(index)?;
        let value = self
            .get_data_property(site.this_value, key)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.delete_own_data_property(site.this_value, key)? {
            return Err(ExecutionError::ReadOnlyProperty(site.this_value));
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(site.this_value, length_atom, safe_integer_value(index))?;
        Ok(value)
    }

    /// Implements the ordinary, non-Proxy `Array.prototype.slice` copy semantics.
    fn array_slice(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let start_value = self.call_argument(site, 0)?.unwrap_or(Value::from_i32(0));
        let end_value = self
            .call_argument(site, 1)?
            .unwrap_or(safe_integer_value(length));
        let start = self.relative_array_index(start_value, length)?;
        let end = self.relative_array_index(end_value, length)?;
        let count = end.saturating_sub(start);
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        for offset in 0..count {
            let source_index = start + offset;
            let source_key = self.safe_integer_property_atom(source_index)?;
            let Some(value) = self.get_data_property(site.this_value, source_key)? else {
                continue;
            };
            let target_key = self.safe_integer_property_atom(offset)?;
            self.set_own_data_property(result, target_key, value)?;
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(result, length_atom, safe_integer_value(count))?;
        Ok(result)
    }

    fn relative_array_index(&mut self, value: Value, length: u64) -> Result<u64, ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?).unwrap_or(f64::NAN);
        if number.is_nan() || number == 0.0 {
            return Ok(0);
        }
        if number.is_sign_negative() {
            return Ok(length.saturating_sub((-number).ceil() as u64));
        }
        Ok(number.floor().min(length as f64) as u64)
    }

    /// Implements `Array.prototype.shift` while preserving holes and generic receiver behavior.
    fn array_shift(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        if length == 0 {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        let first_key = self.safe_integer_property_atom(0)?;
        let first = self
            .get_data_property(site.this_value, first_key)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        for index in 1..length {
            let source_key = self.safe_integer_property_atom(index)?;
            let target_key = self.safe_integer_property_atom(index - 1)?;
            if let Some(value) = self.get_data_property(site.this_value, source_key)? {
                self.set_own_data_property(site.this_value, target_key, value)?;
            } else if !self.delete_own_data_property(site.this_value, target_key)? {
                return Err(ExecutionError::ReadOnlyProperty(site.this_value));
            }
        }
        let last_key = self.safe_integer_property_atom(length - 1)?;
        if !self.delete_own_data_property(site.this_value, last_key)? {
            return Err(ExecutionError::ReadOnlyProperty(site.this_value));
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(site.this_value, length_atom, safe_integer_value(length - 1))?;
        Ok(first)
    }

    /// Implements `Array.prototype.unshift` with backwards indexed movement and exact length.
    fn array_unshift(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let count = u64::from(site.argument_count);
        let new_length = length
            .checked_add(count)
            .filter(|value| *value <= MAX_SAFE_INTEGER)
            .ok_or(ExecutionError::ArrayLengthOverflow)?;
        for index in (0..length).rev() {
            let source_key = self.safe_integer_property_atom(index)?;
            let target_key = self.safe_integer_property_atom(index + count)?;
            if let Some(value) = self.get_data_property(site.this_value, source_key)? {
                self.set_own_data_property(site.this_value, target_key, value)?;
            } else if !self.delete_own_data_property(site.this_value, target_key)? {
                return Err(ExecutionError::ReadOnlyProperty(site.this_value));
            }
        }
        for index in 0..site.argument_count {
            let value = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            let key = self.safe_integer_property_atom(u64::from(index))?;
            self.set_own_data_property(site.this_value, key, value)?;
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(site.this_value, length_atom, safe_integer_value(new_length))?;
        Ok(safe_integer_value(new_length))
    }

    /// Implements `Array.prototype.reverse` by swapping present indexed properties and holes.
    fn array_reverse(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        for lower in 0..(length / 2) {
            let upper = length - lower - 1;
            let lower_key = self.safe_integer_property_atom(lower)?;
            let upper_key = self.safe_integer_property_atom(upper)?;
            let lower_value = self.get_data_property(site.this_value, lower_key)?;
            let upper_value = self.get_data_property(site.this_value, upper_key)?;
            match (lower_value, upper_value) {
                (Some(left), Some(right)) => {
                    self.set_own_data_property(site.this_value, lower_key, right)?;
                    self.set_own_data_property(site.this_value, upper_key, left)?;
                }
                (Some(left), None) => {
                    self.set_own_data_property(site.this_value, upper_key, left)?;
                    if !self.delete_own_data_property(site.this_value, lower_key)? {
                        return Err(ExecutionError::ReadOnlyProperty(site.this_value));
                    }
                }
                (None, Some(right)) => {
                    self.set_own_data_property(site.this_value, lower_key, right)?;
                    if !self.delete_own_data_property(site.this_value, upper_key)? {
                        return Err(ExecutionError::ReadOnlyProperty(site.this_value));
                    }
                }
                (None, None) => {}
            }
        }
        Ok(site.this_value)
    }

    /// Implements `Array.prototype.fill` with ToInteger-relative bounds and hole materialization.
    fn array_fill(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let start_value = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let end_value = self
            .call_argument(site, 2)?
            .unwrap_or(safe_integer_value(length));
        let start = self.relative_array_index(start_value, length)?;
        let end = self.relative_array_index(end_value, length)?;
        for index in start..end {
            let key = self.safe_integer_property_atom(index)?;
            self.set_own_data_property(site.this_value, key, value)?;
        }
        Ok(site.this_value)
    }

    /// Implements `Array.prototype.lastIndexOf` using reverse strict-equality search.
    fn array_last_index_of(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        if length == 0 {
            return Ok(Value::from_i32(-1));
        }
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let from = self
            .call_argument(site, 1)?
            .unwrap_or(safe_integer_value(length - 1));
        let number = numeric_value(self.convert_to_number(from)?).unwrap_or(f64::NAN);
        let mut index = if number.is_nan() {
            length - 1
        } else if number.is_sign_negative() {
            length.saturating_sub((-number).ceil() as u64)
        } else {
            number.floor().min((length - 1) as f64) as u64
        };
        loop {
            let key = self.safe_integer_property_atom(index)?;
            if let Some(value) = self.get_data_property(site.this_value, key)?
                && self.strict_equal_values(value, search)?
            {
                return Ok(safe_integer_value(index));
            }
            if index == 0 {
                break;
            }
            index -= 1;
        }
        Ok(Value::from_i32(-1))
    }

    /// Implements `Array.prototype.copyWithin` with overlap-safe direction and hole preservation.
    fn array_copy_within(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let target_value = self.call_argument(site, 0)?.unwrap_or(Value::from_i32(0));
        let start_value = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let end_value = self
            .call_argument(site, 2)?
            .unwrap_or(safe_integer_value(length));
        let target = self.relative_array_index(target_value, length)?;
        let start = self.relative_array_index(start_value, length)?;
        let end = self.relative_array_index(end_value, length)?;
        let count = end.saturating_sub(start).min(length.saturating_sub(target));
        if target < start || target >= start.saturating_add(count) {
            for offset in 0..count {
                self.copy_within_element(site.this_value, start + offset, target + offset)?;
            }
        } else {
            for offset in (0..count).rev() {
                self.copy_within_element(site.this_value, start + offset, target + offset)?;
            }
        }
        Ok(site.this_value)
    }

    fn copy_within_element(
        &mut self,
        receiver: Value,
        source_index: u64,
        target_index: u64,
    ) -> Result<(), ExecutionError> {
        let source_key = self.safe_integer_property_atom(source_index)?;
        let target_key = self.safe_integer_property_atom(target_index)?;
        if let Some(value) = self.get_data_property(receiver, source_key)? {
            self.set_own_data_property(receiver, target_key, value)?;
        } else if !self.delete_own_data_property(receiver, target_key)? {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        Ok(())
    }

    /// Implements `Array.prototype.flat` with an explicit work stack and bounded depth.
    fn array_flat(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let depth_value = self.call_argument(site, 0)?.unwrap_or(Value::from_i32(1));
        let depth_number = numeric_value(self.convert_to_number(depth_value)?).unwrap_or(f64::NAN);
        let depth = if depth_number.is_nan() || depth_number <= 0.0 {
            0
        } else {
            depth_number.floor().min(u32::MAX as f64) as u32
        };
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        let length = self.length_of_array_like(site.this_value)?;
        let mut work = Vec::new();
        for index in (0..length).rev() {
            let key = self.safe_integer_property_atom(index)?;
            if let Some(value) = self.get_data_property(site.this_value, key)? {
                work.push(FlatWork::Value(value, depth));
            } else {
                work.push(FlatWork::Hole);
            }
        }
        let mut next_index = 0_u64;
        while let Some(item) = work.pop() {
            match item {
                FlatWork::Hole => {
                    continue;
                }
                FlatWork::Value(value, remaining)
                    if remaining > 0 && self.is_array_value(value)? =>
                {
                    let nested_length = self.length_of_array_like(value)?;
                    for index in (0..nested_length).rev() {
                        let key = self.safe_integer_property_atom(index)?;
                        if let Some(nested) = self.get_data_property(value, key)? {
                            work.push(FlatWork::Value(nested, remaining - 1));
                        } else {
                            work.push(FlatWork::Hole);
                        }
                    }
                }
                FlatWork::Value(value, _) => {
                    let key = self.safe_integer_property_atom(next_index)?;
                    self.set_own_data_property(result, key, value)?;
                    next_index = next_index
                        .checked_add(1)
                        .ok_or(ExecutionError::ArrayLengthOverflow)?;
                }
            }
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(result, length_atom, safe_integer_value(next_index))?;
        Ok(result)
    }

    /// Implements the default `Array.prototype.sort` comparator over UTF-16 code units.
    fn array_sort(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let mut values = Vec::new();
        let mut holes = 0_u64;
        for index in 0..length {
            let key = self.safe_integer_property_atom(index)?;
            let Some(value) = self.get_data_property(site.this_value, key)? else {
                holes += 1;
                continue;
            };
            if value.as_immediate() == Some(Immediate::Undefined) {
                values.push((value, None));
                continue;
            }
            let mut units = Vec::new();
            self.append_primitive_string_units(value, &mut units)?;
            values.push((value, Some(units)));
        }
        values.sort_by(|left, right| match (&left.1, &right.1) {
            (None, None) => core::cmp::Ordering::Equal,
            (None, Some(_)) => core::cmp::Ordering::Greater,
            (Some(_), None) => core::cmp::Ordering::Less,
            (Some(left), Some(right)) => left.cmp(right),
        });
        for (index, (value, _)) in values.iter().enumerate() {
            let key = self.safe_integer_property_atom(index as u64)?;
            self.set_own_data_property(site.this_value, key, *value)?;
        }
        for index in (values.len() as u64)..length {
            let key = self.safe_integer_property_atom(index)?;
            if !self.delete_own_data_property(site.this_value, key)? {
                return Err(ExecutionError::ReadOnlyProperty(site.this_value));
            }
        }
        debug_assert_eq!(values.len() as u64 + holes, length);
        Ok(site.this_value)
    }

    fn array_element_or_undefined(
        &mut self,
        receiver: Value,
        index: u64,
    ) -> Result<Value, ExecutionError> {
        let key = self.safe_integer_property_atom(index)?;
        Ok(self
            .get_data_property(receiver, key)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined)))
    }

    #[inline(always)]
    fn same_value_zero(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        if let (Some(left), Some(right)) = (numeric_value(left), numeric_value(right)) {
            return Ok((left.is_nan() && right.is_nan()) || left == right);
        }
        self.strict_equal_values(left, right)
    }

    /// Implements Array.prototype.toString as comma-joined primitive elements for this subset.
    fn array_to_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.join_array_like(receiver, None)
    }

    /// Appends the currently supported ECMAScript primitive string conversion without heap allocation.
    fn append_primitive_string_units(
        &mut self,
        value: Value,
        output: &mut Vec<u16>,
    ) -> Result<(), ExecutionError> {
        if let Some(immediate) = value.as_immediate() {
            let bytes = match immediate {
                Immediate::True => b"true".as_slice(),
                Immediate::False => b"false".as_slice(),
                Immediate::Undefined => b"undefined".as_slice(),
                Immediate::Null => b"null".as_slice(),
                Immediate::Hole | Immediate::Uninitialized => {
                    return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
                }
            };
            output
                .try_reserve(bytes.len())
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            output.extend(bytes.iter().map(|&byte| u16::from(byte)));
            return Ok(());
        }
        if let Some(number) = numeric_value(value) {
            let mut buffer = ryu_js::Buffer::new();
            let printed = if number == 0.0 {
                "0"
            } else {
                buffer.format(number)
            };
            output
                .try_reserve(printed.len())
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            output.extend(printed.bytes().map(u16::from));
            return Ok(());
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedPrimitiveStringConversion(value))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedPrimitiveStringConversion(value))?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                match string.as_view() {
                    JsStringView::Latin1(bytes) => {
                        output
                            .try_reserve(bytes.len())
                            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                        output.extend(bytes.iter().map(|&byte| u16::from(byte)));
                    }
                    JsStringView::Utf16(units) => {
                        output
                            .try_reserve(units.len())
                            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                        output.extend_from_slice(units);
                    }
                }
                Ok(())
            })
        })
    }

    /// Publishes one runtime-created string through the ordinary managed external allocation path.
    fn allocate_runtime_string(&mut self, string: JsString) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let value = self
            .heap
            .try_allocate_external_with_gc(
                self.types.string,
                0,
                string,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(value.raw()))
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

    /// Copies enumerable ordinary data slots in stable shape insertion order.
    fn copy_own_data_properties(
        &mut self,
        target: Value,
        source: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(source) {
            return Ok(());
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        for key in keys {
            if !self
                .shapes
                .lookup(snapshot.shape, key)
                .expect("own key resolves in its source shape")
                .attributes
                .enumerable()
            {
                continue;
            }
            if let Some(value) = self.data_property_from_snapshot(snapshot, key)? {
                self.set_own_data_property(target, key, value)?;
            }
        }
        Ok(())
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

    /// Applies ordinary data descriptors from an Object.create/defineProperties descriptor map.
    fn define_ordinary_properties(
        &mut self,
        target: Value,
        descriptors: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(descriptors) {
            return Err(ExecutionError::NotObject(descriptors));
        }
        let (_, snapshot) = self.object_snapshot(descriptors)?;
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        for key in keys {
            let Some(descriptor) = self.data_property_from_snapshot(snapshot, key)? else {
                continue;
            };
            if !self.is_object_value(descriptor) {
                return Err(ExecutionError::NotObject(descriptor));
            }
            let descriptor = self.parse_data_property_descriptor(descriptor)?;
            self.define_data_property(target, key, descriptor)?;
        }
        Ok(())
    }

    /// Implements Object.assign for ordinary data-property sources and one target object.
    fn object_assign(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let target = if self.is_object_value(target) {
            target
        } else {
            self.create_ordinary_object()?
        };
        for index in 1..site.argument_count {
            let source = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            self.copy_own_data_properties(target, source)?;
        }
        Ok(target)
    }

    /// Materializes Object.keys/values/entries from ordinary enumerable data slots.
    fn object_enumeration(
        &mut self,
        site: &CallSite,
        native: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let source = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        if let Some(raw) = source.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.enumerate_string_primitive(result, string, native);
        }
        if !self.is_object_value(source) {
            return Ok(result);
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        let mut output_index = 0_i32;
        for key in keys {
            if !self
                .shapes
                .lookup(snapshot.shape, key)
                .expect("own key resolves in its source shape")
                .attributes
                .enumerable()
            {
                continue;
            }
            let Some(value) = self.data_property_from_snapshot(snapshot, key)? else {
                continue;
            };
            match native {
                NativeFunction::ObjectEntries => {
                    self.append_object_entry(result, output_index, key, value)?;
                }
                NativeFunction::ObjectKeys => {
                    let key_value = self.atom_string_value(key)?;
                    self.append_object_enumeration_item(result, output_index, key_value, native)?;
                }
                NativeFunction::ObjectValues => {
                    self.append_object_enumeration_item(result, output_index, value, native)?;
                }
                _ => return Err(ExecutionError::NonCallable(source)),
            }
            output_index = output_index
                .checked_add(1)
                .ok_or(ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(result, length, Value::from_i32(output_index))?;
        Ok(result)
    }

    /// Materializes all present own string keys, including non-enumerable properties.
    fn object_get_own_property_names(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let source = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        if !self.is_object_value(source) {
            return Ok(result);
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        let mut output_index = 0_u64;
        for key in keys {
            if self.data_property_from_snapshot(snapshot, key)?.is_none() {
                continue;
            }
            let name = self.atom_string_value(key)?;
            let output_key = self.safe_integer_property_atom(output_index)?;
            self.set_own_data_property(result, output_key, name)?;
            output_index = output_index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
        }
        if self.resolve_function_object(source).is_ok() {
            for key in [self.length_atom()?, self.name_atom()?] {
                if self.shapes.lookup(snapshot.shape, key).is_some() {
                    continue;
                }
                let name = self.atom_string_value(key)?;
                let output_key = self.safe_integer_property_atom(output_index)?;
                self.set_own_data_property(result, output_key, name)?;
                output_index = output_index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
            }
            let prototype = self.prototype_atom()?;
            if self.is_function_prototype_property(source, prototype)
                && self.shapes.lookup(snapshot.shape, prototype).is_none()
            {
                let name = self.atom_string_value(prototype)?;
                let output_key = self.safe_integer_property_atom(output_index)?;
                self.set_own_data_property(result, output_key, name)?;
                output_index = output_index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
            }
        }
        let length = self.length_atom()?;
        self.set_own_data_property(result, length, safe_integer_value(output_index))?;
        Ok(result)
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

    /// Enumerates the virtual indexed properties exposed by one primitive string.
    fn enumerate_string_primitive(
        &mut self,
        result: Value,
        string: GcRef<JsString>,
        native: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let units = self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok::<Vec<u16>, ExecutionError>(match string.as_view() {
                    JsStringView::Latin1(bytes) => {
                        bytes.iter().map(|&byte| u16::from(byte)).collect()
                    }
                    JsStringView::Utf16(units) => units.to_vec(),
                })
            })
        })?;
        let length = i32::try_from(units.len())
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
        for (index, unit) in units.into_iter().enumerate() {
            let index = i32::try_from(index)
                .map_err(|_| ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
            let key_atom = self.property_key_atom(Value::from_i32(index))?;
            match native {
                NativeFunction::ObjectEntries => {
                    let pair = self.create_and_root_entry_pair(result, index)?;
                    let zero = self.property_key_atom(Value::from_i32(0))?;
                    let key = self.atom_string_value(key_atom)?;
                    self.set_own_data_property(pair, zero, key)?;
                    let one = self.property_key_atom(Value::from_i32(1))?;
                    let value = self.allocate_runtime_string(
                        JsString::try_from_utf16(&[unit])
                            .map_err(ExecutionError::PropertyKeyString)?,
                    )?;
                    self.set_own_data_property(pair, one, value)?;
                    let pair_length = self.intern_intrinsic_name(b"length")?;
                    self.set_own_data_property(pair, pair_length, Value::from_i32(2))?;
                }
                NativeFunction::ObjectKeys => {
                    let key = self.atom_string_value(key_atom)?;
                    self.append_object_enumeration_item(result, index, key, native)?;
                }
                NativeFunction::ObjectValues => {
                    let value = self.allocate_runtime_string(
                        JsString::try_from_utf16(&[unit])
                            .map_err(ExecutionError::PropertyKeyString)?,
                    )?;
                    self.append_object_enumeration_item(result, index, value, native)?;
                }
                _ => return Err(ExecutionError::NonCallable(result)),
            }
        }
        let length_atom = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(result, length_atom, Value::from_i32(length))?;
        Ok(result)
    }

    /// Appends one materialized key/value/entry without duplicating Array pair construction.
    fn append_object_enumeration_item(
        &mut self,
        result: Value,
        output_index: i32,
        item: Value,
        native: NativeFunction,
    ) -> Result<(), ExecutionError> {
        debug_assert!(matches!(
            native,
            NativeFunction::ObjectKeys | NativeFunction::ObjectValues
        ));
        let result_key = self.property_key_atom(Value::from_i32(output_index))?;
        self.set_own_data_property(result, result_key, item)
    }

    /// Creates and roots an Object.entries pair before allocating its key string.
    fn create_and_root_entry_pair(
        &mut self,
        result: Value,
        output_index: i32,
    ) -> Result<Value, ExecutionError> {
        let pair = self.create_unrooted_array()?;
        let pair_key = self.property_key_atom(Value::from_i32(output_index))?;
        self.set_own_data_property(result, pair_key, pair)?;
        Ok(pair)
    }

    /// Appends one Object.entries pair whose source value remains rooted by the source object.
    fn append_object_entry(
        &mut self,
        result: Value,
        output_index: i32,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pair = self.create_and_root_entry_pair(result, output_index)?;
        let zero = self.property_key_atom(Value::from_i32(0))?;
        let key = self.atom_string_value(key)?;
        self.set_own_data_property(pair, zero, key)?;
        let one = self.property_key_atom(Value::from_i32(1))?;
        self.set_own_data_property(pair, one, value)?;
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(pair, length, Value::from_i32(2))
    }

    /// Allocates an empty Array value whose caller immediately publishes it into a rooted owner.
    fn create_unrooted_array(&mut self) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before Object.entries");
        self.create_array_object_with_prototype(prototype)
    }

    /// Copies an immortal atom spelling into one GC-managed ECMAScript string value.
    fn atom_string_value(&mut self, atom: AtomId) -> Result<Value, ExecutionError> {
        let string = self
            .atoms
            .get(atom)
            .expect("shape keys always reference live isolate atoms");
        let string = match string.as_view() {
            JsStringView::Latin1(bytes) => JsString::try_from_latin1(bytes),
            JsStringView::Utf16(units) => JsString::try_from_utf16(units),
        }
        .map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(string)
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

    /// Enumerates this isolate's fiber roots for a stop-the-world collection safepoint.
    ///
    /// The collector supplies a rewrite-capable tracer. This API does not resolve logical addresses
    /// or borrow heap objects, so it remains valid across non-moving collection phases.
    pub fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        self.finalization_jobs.trace(tracer);
        self.realm.trace(tracer);
        for code in &mut self.loaded_code {
            code.trace(tracer);
        }
    }

    /// Starts the module entry function with one checked register reservation before opcode dispatch.
    pub fn execute(
        &mut self,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let code = self.load_module(module)?;
        self.execute_loaded(code, budget)
    }

    /// Resolves immutable scope names once and publishes one bounded isolate-local code entry.
    pub fn load_module(&mut self, module: &CompiledModule) -> Result<CodeId, ExecutionError> {
        if let Some(index) = self
            .loaded_code
            .iter()
            .position(|loaded| loaded.module.ptr_eq(module))
        {
            return CodeId::from_index(index)
                .ok_or(ExecutionError::LoadedModuleLimit { limit: u32::MAX });
        }
        if self.loaded_code.len() >= self.realm.limits.max_loaded_modules as usize {
            return Err(ExecutionError::LoadedModuleLimit {
                limit: self.realm.limits.max_loaded_modules,
            });
        }
        self.loaded_code
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::LoadedCodeAllocationFailed)?;
        let mut scope_resolutions = Vec::new();
        scope_resolutions
            .try_reserve_exact(module.scope_names().len())
            .map_err(|_| ExecutionError::ScopeNameAllocationFailed)?;
        let checkpoint = self.atoms.checkpoint();
        for name in module.scope_names() {
            let string = match JsString::try_from_str(name) {
                Ok(string) => string,
                Err(error) => {
                    self.atoms.rollback(checkpoint);
                    return Err(ExecutionError::ScopeNameString(error));
                }
            };
            match self.atoms.try_intern(string) {
                Ok(atom) => scope_resolutions.push(ScopeResolution {
                    atom,
                    lexical_slot: self.realm.resolve_lexical(atom),
                    intrinsic_slot: self.realm.resolve_intrinsic(atom),
                    global_slot: self.realm.resolve(atom),
                }),
                Err(error) => {
                    self.atoms.rollback(checkpoint);
                    return Err(ExecutionError::ScopeNameAtom(error));
                }
            }
        }
        let mut constant_values = Vec::new();
        if constant_values
            .try_reserve_exact(module.constants().len())
            .is_err()
        {
            self.atoms.rollback(checkpoint);
            return Err(ExecutionError::ConstantValueAllocationFailed);
        }
        for constant in module.constants() {
            let value = match constant {
                BytecodeConstant::String(code_units) => {
                    let string = match JsString::try_from_utf16(code_units) {
                        Ok(string) => string,
                        Err(error) => {
                            self.atoms.rollback(checkpoint);
                            return Err(ExecutionError::ConstantString(error));
                        }
                    };
                    let mut roots = CodeLoadRoots {
                        vm: VmRoots {
                            fiber: &mut self.fiber,
                            finalization_jobs: &mut self.finalization_jobs,
                            realm: &mut self.realm,
                            loaded_code: &mut self.loaded_code,
                        },
                        constant_values: &mut constant_values,
                    };
                    match self.heap.try_allocate_external_with_gc(
                        self.types.string,
                        0,
                        string,
                        AllocationSpace::Young,
                        &mut roots,
                    ) {
                        Ok(reference) => Some(Value::from_heap_ref(reference.raw())),
                        Err(error) => {
                            self.atoms.rollback(checkpoint);
                            return Err(ExecutionError::HeapAllocation(error));
                        }
                    }
                }
                _ => None,
            };
            constant_values.push(value);
        }
        let code = CodeId::from_index(self.loaded_code.len())
            .ok_or(ExecutionError::LoadedModuleLimit { limit: u32::MAX })?;
        self.loaded_code.push(LoadedCode {
            module: module.clone(),
            scope_resolutions: scope_resolutions.into_boxed_slice(),
            constant_values: constant_values.into_boxed_slice(),
        });
        Ok(code)
    }

    /// Executes already-loaded code without repeating module identity or scope-name resolution.
    pub fn execute_loaded(
        &mut self,
        code: CodeId,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        self.execute_loaded_with_batch::<{ tuning::dispatch::DEFAULT_DISPATCH_BATCH }>(code, budget)
    }

    #[inline(always)]
    fn loaded_code(&self, code: CodeId) -> Result<&LoadedCode, ExecutionError> {
        self.loaded_code
            .get(code.index())
            .ok_or(ExecutionError::InvalidCode(code))
    }

    /// Resolves an immutable scope atom once, then retains its stable global slot in loaded code.
    #[inline(always)]
    fn scope_resolution(
        &mut self,
        code: CodeId,
        scope_name: u32,
    ) -> Result<ScopeResolution, ExecutionError> {
        let resolution = self
            .loaded_code(code)?
            .scope_resolutions
            .get(scope_name as usize)
            .copied()
            .ok_or(ExecutionError::InvalidScopeName { code, scope_name })?;
        if resolution.lexical_slot.is_some()
            || resolution.intrinsic_slot.is_some()
            || resolution.global_slot.is_some()
        {
            return Ok(resolution);
        }
        let lexical_slot = self.realm.resolve_lexical(resolution.atom);
        let intrinsic_slot = self.realm.resolve_intrinsic(resolution.atom);
        let global_slot = self.realm.resolve(resolution.atom);
        if lexical_slot.is_none() && intrinsic_slot.is_none() && global_slot.is_none() {
            return Ok(resolution);
        }
        let resolved = ScopeResolution {
            lexical_slot,
            intrinsic_slot,
            global_slot,
            ..resolution
        };
        self.loaded_code
            .get_mut(code.index())
            .expect("validated loaded code remains present")
            .scope_resolutions[scope_name as usize] = resolved;
        Ok(resolved)
    }

    #[inline(always)]
    fn scope_atom(&self, code: CodeId, scope_name: u32) -> Result<AtomId, ExecutionError> {
        self.loaded_code(code)?
            .scope_resolutions
            .get(scope_name as usize)
            .map(|resolution| resolution.atom)
            .ok_or(ExecutionError::InvalidScopeName { code, scope_name })
    }

    /// Converts supported primitive values to interned PropertyKeys.
    #[cold]
    fn property_key_atom(&mut self, value: Value) -> Result<AtomId, ExecutionError> {
        if let Some(raw) = value.as_heap_ref()
            && let Ok(reference) = self.heap.checked_reference(raw, self.types.string)
        {
            let string = self.heap.with_running_scope(|scope| {
                let root = scope.root(reference).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let string = no_gc
                        .borrow(root, self.types.string)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    match string.as_view() {
                        JsStringView::Latin1(bytes) => JsString::try_from_latin1(bytes),
                        JsStringView::Utf16(code_units) => JsString::try_from_utf16(code_units),
                    }
                    .map_err(ExecutionError::PropertyKeyString)
                })
            })?;
            return self
                .atoms
                .try_intern(string)
                .map_err(ExecutionError::PropertyKeyAtom);
        }
        if let Some(integer) = value.as_i32() {
            let key = Int32PropertyKey::new(integer);
            if let Some(atom) = self.atoms.find_latin1(key.as_bytes()) {
                return Ok(atom);
            }
            let string = JsString::try_from_latin1(key.as_bytes())
                .map_err(ExecutionError::PropertyKeyString)?;
            return self
                .atoms
                .try_intern(string)
                .map_err(ExecutionError::PropertyKeyAtom);
        }

        // Number::toString is only reached after the immediate integer fast path.
        let number = value
            .as_f64()
            .ok_or(ExecutionError::UnsupportedPropertyKey(value))?;
        let mut buffer = ryu_js::Buffer::new();
        let printed = if number == 0.0 {
            "0"
        } else {
            buffer.format(number)
        };
        let string = JsString::try_from_str(printed).map_err(ExecutionError::PropertyKeyString)?;
        self.atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)
    }

    /// Converts the primitive values represented by the current numeric VM subset.
    #[inline(always)]
    fn convert_to_number(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(value);
        }
        match value.as_immediate() {
            Some(Immediate::True) => Ok(Value::from_i32(1)),
            Some(Immediate::False | Immediate::Null) => Ok(Value::from_i32(0)),
            Some(Immediate::Undefined) => Ok(Value::from_f64(f64::NAN)),
            Some(Immediate::Hole | Immediate::Uninitialized) => {
                Err(ExecutionError::UnsupportedNumberConversion(value))
            }
            None => {
                let raw = value
                    .as_heap_ref()
                    .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
                if self.heap.checked_reference(raw, self.types.symbol).is_ok() {
                    return Err(ExecutionError::NotObject(value));
                }
                let Ok(reference) = self.heap.checked_reference(raw, self.types.string) else {
                    if self.is_object_value(value) {
                        let value_of = self.intern_intrinsic_name(b"valueOf")?;
                        let to_string = self.intern_intrinsic_name(b"toString")?;
                        let value_of = self.get_data_property(value, value_of)?;
                        let to_string = self.get_data_property(value, to_string)?;
                        let has_callable = [value_of, to_string]
                            .into_iter()
                            .flatten()
                            .any(|method| self.resolve_function_object(method).is_ok());
                        if !has_callable {
                            return Err(ExecutionError::NotObject(value));
                        }
                    }
                    return Err(ExecutionError::UnsupportedNumberConversion(value));
                };
                let units = self.heap.with_running_scope(|scope| {
                    let root = scope.root(reference).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let string = no_gc
                            .borrow(root, self.types.string)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        let units = match string.as_view() {
                            JsStringView::Latin1(bytes) => {
                                bytes.iter().map(|&byte| u16::from(byte)).collect()
                            }
                            JsStringView::Utf16(units) => units.to_vec(),
                        };
                        Ok::<_, ExecutionError>(units)
                    })
                })?;
                Ok(Value::from_f64(parse_number_code_units(&units)))
            }
        }
    }

    #[inline(always)]
    fn typeof_value(&self, value: Value) -> Result<Value, ExecutionError> {
        let strings = self.realm.typeof_strings;
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(strings.number);
        }
        if let Some(immediate) = value.as_immediate() {
            return match immediate {
                Immediate::Undefined => Ok(strings.undefined),
                Immediate::Null => Ok(strings.object),
                Immediate::False | Immediate::True => Ok(strings.boolean),
                Immediate::Hole | Immediate::Uninitialized => {
                    Err(ExecutionError::UnsupportedTypeof(value))
                }
            };
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedTypeof(value))?;
        if self.heap.checked_reference(raw, self.types.string).is_ok() {
            return Ok(strings.string);
        }
        if self.heap.checked_reference(raw, self.types.symbol).is_ok() {
            return Ok(strings.symbol);
        }
        if self
            .heap
            .checked_reference(raw, self.types.function)
            .is_ok()
        {
            return Ok(strings.function);
        }
        if self
            .heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
        {
            return Ok(strings.object);
        }
        Err(ExecutionError::UnsupportedTypeof(value))
    }

    /// Implements SameValue, including NaN equality and signed-zero distinction.
    fn same_value(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        if let (Some(left), Some(right)) = (numeric_value(left), numeric_value(right)) {
            if left.is_nan() && right.is_nan() {
                return Ok(true);
            }
            if left == 0.0 && right == 0.0 {
                return Ok(left.is_sign_negative() == right.is_sign_negative());
            }
            return Ok(left == right);
        }
        self.strict_equal_values(left, right)
    }

    /// Applies strict equality without allocating while preserving numeric and string semantics.
    fn strict_equal_values(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        match (numeric_value(left), numeric_value(right)) {
            (Some(left), Some(right)) => return Ok(left == right),
            (Some(_), None) | (None, Some(_)) => return Ok(false),
            (None, None) => {}
        }
        if left == right {
            return Ok(true);
        }
        let (Some(left), Some(right)) = (left.as_heap_ref(), right.as_heap_ref()) else {
            return Ok(false);
        };
        let Ok(left) = self.heap.checked_reference(left, self.types.string) else {
            return Ok(false);
        };
        let Ok(right) = self.heap.checked_reference(right, self.types.string) else {
            return Ok(false);
        };
        self.heap.with_running_scope(|scope| {
            let left = scope.root(left).map_err(ExecutionError::Root)?;
            let right = scope.root(right).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let left = no_gc
                    .borrow(left, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let right = no_gc
                    .borrow(right, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(left == right)
            })
        })
    }

    /// Implements the supported primitive subset of Abstract Equality Comparison.
    fn loose_equal_values(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        if self.strict_equal_values(left, right)? {
            return Ok(true);
        }
        let left_immediate = left.as_immediate();
        let right_immediate = right.as_immediate();
        let left_nullish = matches!(left_immediate, Some(Immediate::Undefined | Immediate::Null));
        let right_nullish = matches!(
            right_immediate,
            Some(Immediate::Undefined | Immediate::Null)
        );
        if left_nullish || right_nullish {
            return Ok(left_nullish && right_nullish);
        }
        let left_number = numeric_value(left);
        let right_number = numeric_value(right);
        if left_number.is_some() && right_number.is_some() {
            return Ok(left_number == right_number);
        }
        let left_boolean = matches!(left_immediate, Some(Immediate::True | Immediate::False));
        let right_boolean = matches!(right_immediate, Some(Immediate::True | Immediate::False));
        if left_boolean || right_boolean || left_number.is_some() || right_number.is_some() {
            let left = self.convert_to_number(left)?;
            let right = self.convert_to_number(right)?;
            return Ok(numeric_value(left) == numeric_value(right));
        }
        Ok(false)
    }

    #[inline(always)]
    fn is_truthy_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if let Some(raw) = value.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(string, self.types.string)
                        .map(|string| !string.is_empty())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            });
        }
        Ok(is_non_string_truthy(value))
    }

    /// Executes with a fixed internal batch size so each monomorphization preserves the same fuel contract.
    #[cfg(test)]
    fn execute_with_batch<const N: usize>(
        &mut self,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let code = self.load_module(module)?;
        self.execute_loaded_with_batch::<N>(code, budget)
    }

    /// Runs one resolved code entry with a test-selectable dispatch batch monomorphization.
    fn execute_loaded_with_batch<const N: usize>(
        &mut self,
        code: CodeId,
        mut budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        if N == 0 {
            return Err(ExecutionError::InvalidDispatchBatch { batch: N });
        }
        let entry_function = self.loaded_code(code)?.module.entry_function();
        self.enter(code, entry_function)?;
        loop {
            if budget.fuel == 0 || budget.quantum == 0 {
                return Ok(RunOutcome::BudgetExhausted);
            }
            if let Some(outcome) = self.execute_batch::<N>(&mut budget)? {
                return Ok(outcome);
            }
        }
    }

    /// The `N` parameter is an internal dispatch-tuning knob; every executed opcode still consumes exact fuel.
    #[inline(always)]
    fn execute_batch<const N: usize>(
        &mut self,
        budget: &mut ExecutionBudget,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let cached_frame = *self
            .fiber
            .frames
            .last()
            .expect("entry frame exists while executing");
        let bytecode = self
            .loaded_code(cached_frame.code)?
            .module
            .function(cached_frame.function)
            .ok_or(ExecutionError::MissingEntryFunction(cached_frame.function))?
            .bytecode()
            .bytecode();
        let cursor = BytecodeCursor::new(bytecode.words());
        for _ in 0..N {
            if budget.fuel == 0 || budget.quantum == 0 {
                return Ok(Some(RunOutcome::BudgetExhausted));
            }
            let frame = *self
                .fiber
                .frames
                .last()
                .expect("entry frame exists while executing");
            if frame.code != cached_frame.code || frame.function != cached_frame.function {
                return Ok(None);
            }
            let instruction = cursor
                .decode(frame.pc)
                .map_err(|_| ExecutionError::DecodeInvariant(frame.pc))?;
            let next_pc = WordOffset::new(frame.pc.index() + u32::from(instruction.word_len));
            self.fiber
                .frames
                .last_mut()
                .expect("frame remains active")
                .pc = next_pc;
            budget.fuel -= 1;
            budget.quantum -= 1;
            let outcome = match self.dispatch(
                frame.code,
                frame.pc,
                instruction.opcode,
                instruction.operands,
                frame.base,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let Some(kind) = execution_error_kind(&error) else {
                        return Err(error);
                    };
                    self.throw_native_error(kind, frame.pc)?
                }
            };
            if let Some(outcome) = outcome {
                return Ok(Some(outcome));
            }
        }
        Ok(None)
    }

    /// Implements one verified opcode without conflating engine faults with language exceptions.
    #[inline(always)]
    fn dispatch(
        &mut self,
        code: CodeId,
        instruction_offset: WordOffset,
        opcode: Opcode,
        operands: [u32; 3],
        base: u32,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match opcode {
            Opcode::Nop => {}
            Opcode::LoadUndefined => self.write(
                base,
                operands[0],
                Value::from_immediate(Immediate::Undefined),
            )?,
            Opcode::LoadNull => {
                self.write(base, operands[0], Value::from_immediate(Immediate::Null))?
            }
            Opcode::LoadFalse => {
                self.write(base, operands[0], Value::from_immediate(Immediate::False))?
            }
            Opcode::LoadTrue => {
                self.write(base, operands[0], Value::from_immediate(Immediate::True))?
            }
            Opcode::LoadImmediate => {
                self.write(base, operands[0], Value::from_i32(operands[1] as i32))?
            }
            Opcode::LoadConstant => {
                let constant_index = operands[1] as usize;
                let loaded = self.loaded_code(code)?;
                let constant = loaded
                    .module
                    .constants()
                    .get(constant_index)
                    .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?;
                let value = match constant {
                    BytecodeConstant::NumberBits(bits) => Value::from_f64(f64::from_bits(*bits)),
                    BytecodeConstant::String(_) => loaded
                        .constant_values
                        .get(constant_index)
                        .copied()
                        .flatten()
                        .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?,
                    _ => return Err(ExecutionError::UnsupportedConstant(operands[1])),
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Move => {
                let value = self.read(base, operands[1])?;
                self.write(base, operands[0], value)?;
            }
            Opcode::Not => {
                let value = if self.is_truthy_value(self.read(base, operands[1])?)? {
                    Value::from_immediate(Immediate::False)
                } else {
                    Value::from_immediate(Immediate::True)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Negate => {
                let input = self.read(base, operands[1])?;
                if self.is_object_value(input) {
                    self.dispatch_object_numeric_conversion(
                        ConversionConsumer::Negate,
                        base,
                        operands[0],
                        Value::from_immediate(Immediate::Undefined),
                        input,
                        instruction_offset,
                    )?;
                } else {
                    let value = numeric_negate(self.convert_to_number(input)?);
                    self.write(base, operands[0], value)?;
                }
            }
            Opcode::ToNumber => {
                let input = self.read(base, operands[1])?;
                if self.is_object_value(input) {
                    self.dispatch_object_numeric_conversion(
                        ConversionConsumer::ToNumber,
                        base,
                        operands[0],
                        Value::from_immediate(Immediate::Undefined),
                        input,
                        instruction_offset,
                    )?;
                } else {
                    let value = self.convert_to_number(input)?;
                    self.write(base, operands[0], value)?;
                }
            }
            Opcode::BitwiseNot => {
                let input = self.read(base, operands[1])?;
                if self.is_object_value(input) {
                    self.dispatch_object_numeric_conversion(
                        ConversionConsumer::BitwiseNot,
                        base,
                        operands[0],
                        Value::from_immediate(Immediate::Undefined),
                        input,
                        instruction_offset,
                    )?;
                } else {
                    let number = self.convert_to_number(input)?;
                    self.write(base, operands[0], numeric_bitwise_not(number))?;
                }
            }
            Opcode::Add => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                self.write(base, operands[0], numeric_binary(opcode, left, right))?;
            }
            Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::BitwiseAnd
            | Opcode::BitwiseOr
            | Opcode::BitwiseXor
            | Opcode::ShiftLeft
            | Opcode::ShiftRight
            | Opcode::ShiftRightUnsigned
            | Opcode::Remainder
            | Opcode::Exponentiate => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                if self.is_object_value(left) {
                    self.dispatch_object_numeric_conversion(
                        ConversionConsumer::BinaryLeft(opcode),
                        base,
                        operands[0],
                        right,
                        left,
                        instruction_offset,
                    )?;
                } else {
                    let left = self.convert_to_number(left)?;
                    if self.is_object_value(right) {
                        self.dispatch_object_numeric_conversion(
                            ConversionConsumer::BinaryRight(opcode),
                            base,
                            operands[0],
                            left,
                            right,
                            instruction_offset,
                        )?;
                    } else {
                        let right = self.convert_to_number(right)?;
                        self.write(
                            base,
                            operands[0],
                            numeric_binary_operation(opcode, left, right),
                        )?;
                    }
                }
            }
            Opcode::GreaterThan | Opcode::LessEqual | Opcode::GreaterEqual => {
                let left = self.convert_to_number(self.read(base, operands[1])?)?;
                let right = self.convert_to_number(self.read(base, operands[2])?)?;
                self.write(base, operands[0], numeric_relational(opcode, left, right))?;
            }
            Opcode::StrictEqual => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if self.strict_equal_values(left, right)? {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::LooseEqual | Opcode::LooseNotEqual => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let equal = self.loose_equal_values(left, right)?;
                let result = if opcode == Opcode::LooseEqual {
                    equal
                } else {
                    !equal
                };
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::HasProperty => {
                let key = self.property_key_atom(self.read(base, operands[1])?)?;
                let receiver = self.read(base, operands[2])?;
                let result = self.get_data_property(receiver, key)?.is_some();
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::TypeofScope => {
                let resolution = self.scope_resolution(code, operands[1])?;
                let value = self
                    .scope_value(resolution)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                let value = self.typeof_value(value)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::DeleteById => {
                let receiver = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                let result = self.delete_data_property_from_bytecode(receiver, key)?;
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::DeleteByValue => {
                let receiver = self.read(base, operands[1])?;
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
                let result = self.delete_data_property_from_bytecode(receiver, key)?;
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::Typeof => {
                let value = self.typeof_value(self.read(base, operands[1])?)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::LessThan => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if numeric_less_than(left, right) {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::InstanceOf => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if self.ordinary_instance_of(left, right)? {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Jump => self.set_pc(WordOffset::new(operands[0])),
            Opcode::JumpIfFalse => {
                if !self.is_truthy_value(self.read(base, operands[0])?)? {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::JumpIfTrue => {
                if self.is_truthy_value(self.read(base, operands[0])?)? {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::JumpIfNotNullish => {
                if !is_nullish(self.read(base, operands[0])?) {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::LoadScope => {
                let resolution = self.scope_resolution(code, operands[1])?;
                let value = self
                    .scope_value(resolution)?
                    .ok_or(ExecutionError::UnresolvedBinding(resolution.atom))?;
                self.write(base, operands[0], value)?;
            }
            Opcode::StoreScope => {
                let value = self.read(base, operands[0])?;
                self.store_scope(code, operands[1], value)?;
            }
            Opcode::StoreResolvedScope => {
                let value = self.read(base, operands[0])?;
                self.store_resolved_scope(code, operands[1], value)?;
            }
            Opcode::LoadEnvironment => {
                let value = self.load_environment(operands[1], operands[2])?;
                self.write(base, operands[0], value)?;
            }
            Opcode::StoreEnvironment => {
                let value = self.read(base, operands[0])?;
                self.store_environment(operands[1], operands[2], value)?;
            }
            Opcode::DeclareScope => {
                self.declare_scope(code, operands[0])?;
            }
            Opcode::DeclareGlobalLexical => {
                self.declare_global_lexical(code, operands[0], operands[1] != 0)?;
            }
            Opcode::InitializeGlobalLexical => {
                let value = self.read(base, operands[0])?;
                self.initialize_global_lexical(code, operands[1], value)?;
            }
            Opcode::CreateClosure => {
                self.create_closure(code, base, operands[0], FunctionId::new(operands[1]))?
            }
            Opcode::CreateObject => {
                let object = self.create_ordinary_object()?;
                self.write(base, operands[0], object)?;
            }
            Opcode::CreateArray => {
                let prototype = self
                    .realm
                    .array_prototype
                    .expect("Array prototype initializes before array literals");
                let object = self.create_array_object_with_prototype(prototype)?;
                self.write(base, operands[0], object)?;
            }
            Opcode::CreateForInIterator => {
                let source = self.read(base, operands[1])?;
                let iterator = self.create_for_in_iterator(source)?;
                self.write(base, operands[0], iterator)?;
            }
            Opcode::ForInNext => {
                let iterator = self.read(base, operands[1])?;
                let value = self.for_in_next(iterator)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadException => {
                let value = self
                    .fiber
                    .pending_exception
                    .take()
                    .ok_or(ExecutionError::MissingPendingException)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadThis => {
                let value = self
                    .fiber
                    .frames
                    .last()
                    .expect("this load always has an active frame")
                    .this_value;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadNewTarget => {
                let value = self
                    .fiber
                    .frames
                    .last()
                    .expect("new.target load always has an active frame")
                    .new_target;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadArgumentsLength => {
                let length = self
                    .fiber
                    .frames
                    .last()
                    .expect("arguments length load always has an active frame")
                    .argument_count;
                self.write(base, operands[0], safe_integer_value(u64::from(length)))?;
            }
            Opcode::GetById => {
                let receiver = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                let value = self
                    .get_data_property(receiver, key)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.write(base, operands[0], value)?;
            }
            Opcode::SetById => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                self.set_data_property_from_bytecode(receiver, key, value)?;
            }
            Opcode::GetByValue => {
                let receiver = self.read(base, operands[1])?;
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
                let value = self
                    .get_data_property(receiver, key)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.write(base, operands[0], value)?;
            }
            Opcode::SetByValue => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
                self.set_data_property_from_bytecode(receiver, key, value)?;
            }
            Opcode::Call => {
                let callee = self.read(base, operands[1])?;
                self.call(CallSite {
                    caller_base: base,
                    destination: operands[0],
                    callee,
                    argument_base: base
                        .checked_add(operands[1])
                        .and_then(|base| base.checked_add(1))
                        .ok_or(ExecutionError::RegisterWindowTooLarge(operands[2]))?,
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: operands[2],
                    this_value: Value::from_immediate(Immediate::Undefined),
                    new_target: Value::from_immediate(Immediate::Undefined),
                    construct_receiver: None,
                    call_site: instruction_offset,
                })?;
            }
            Opcode::CallWithReceiver => {
                let receiver = self.read(base, operands[1])?;
                let callee = self.read(base, operands[1] + 1)?;
                self.call(CallSite {
                    caller_base: base,
                    destination: operands[0],
                    callee,
                    argument_base: base
                        .checked_add(operands[1])
                        .and_then(|base| base.checked_add(2))
                        .ok_or(ExecutionError::RegisterWindowTooLarge(operands[2]))?,
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: operands[2],
                    this_value: receiver,
                    new_target: Value::from_immediate(Immediate::Undefined),
                    construct_receiver: None,
                    call_site: instruction_offset,
                })?;
            }
            Opcode::Construct => self.construct(
                base,
                operands[0],
                operands[1],
                operands[2],
                instruction_offset,
            )?,
            Opcode::Return => {
                let value = self.read(base, operands[0])?;
                return self.finish_return(value);
            }
            Opcode::ReturnUndefined => {
                let value = Value::from_immediate(Immediate::Undefined);
                return self.finish_return(value);
            }
            Opcode::Throw => {
                let value = self.read(base, operands[0])?;
                return self.throw_value(value, instruction_offset);
            }
            _ => return Err(ExecutionError::UnsupportedOpcode(opcode)),
        }
        Ok(None)
    }

    #[cold]
    #[inline(never)]
    fn throw_native_error(
        &mut self,
        kind: NativeErrorKind,
        instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let error = self.create_native_error(kind, None)?;
        self.throw_value(error, instruction_offset)
    }

    #[inline(always)]
    fn scope_value(&self, resolution: ScopeResolution) -> Result<Option<Value>, ExecutionError> {
        if let Some(slot) = resolution.lexical_slot {
            return self.realm.lexical_value(slot).map(Some);
        }
        if let Some(slot) = resolution.intrinsic_slot {
            return Ok(Some(self.realm.intrinsic_value(slot)));
        }
        Ok(resolution
            .global_slot
            .and_then(|slot| self.realm.get_slot(slot)))
    }

    /// Writes through a cached global slot or publishes the binding once on the cold path.
    #[inline(always)]
    fn store_scope(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if let Some(slot) = resolution.intrinsic_slot {
            return self.realm.set_intrinsic(slot, value);
        }
        if let Some(slot) = resolution.global_slot {
            self.realm.set_slot(slot, value);
            return Ok(());
        }
        self.realm.set(resolution.atom, value)
    }

    #[inline(always)]
    fn declare_scope(&mut self, code: CodeId, scope_name: u32) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        self.declare_scope_resolution(resolution)
    }

    #[inline(always)]
    fn declare_scope_resolution(
        &mut self,
        resolution: ScopeResolution,
    ) -> Result<(), ExecutionError> {
        if resolution.lexical_slot.is_some() {
            return Err(ExecutionError::GlobalLexicalRedeclaration(resolution.atom));
        }
        if self.scope_value(resolution)?.is_some() {
            return Ok(());
        }
        self.realm
            .set(resolution.atom, Value::from_immediate(Immediate::Undefined))
    }

    /// Updates a mutable global or applies the strict/sloppy primitive-intrinsic write contract.
    fn store_resolved_scope(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if let Some(slot) = resolution.lexical_slot {
            return self.realm.set_lexical(slot, value);
        }
        if let Some(slot) = resolution.intrinsic_slot {
            let strict = self
                .fiber
                .frames
                .last()
                .is_some_and(|frame| frame.strictness == FunctionStrictness::Strict);
            return match self.realm.set_intrinsic(slot, value) {
                Err(ExecutionError::ReadOnlyBinding(_)) if !strict => Ok(()),
                result => result,
            };
        }
        if let Some(slot) = resolution.global_slot {
            self.realm.set_slot(slot, value);
            return Ok(());
        }
        let strict = self
            .fiber
            .frames
            .last()
            .is_some_and(|frame| frame.strictness == FunctionStrictness::Strict);
        if strict {
            Err(ExecutionError::UnresolvedBinding(resolution.atom))
        } else {
            self.realm.set(resolution.atom, value)
        }
    }

    fn declare_global_lexical(
        &mut self,
        code: CodeId,
        scope_name: u32,
        mutable: bool,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if resolution.lexical_slot.is_some()
            || resolution.intrinsic_slot.is_some()
            || resolution.global_slot.is_some()
        {
            return Err(ExecutionError::GlobalLexicalRedeclaration(resolution.atom));
        }
        self.realm.declare_lexical(resolution.atom, mutable)
    }

    fn initialize_global_lexical(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        let slot = resolution
            .lexical_slot
            .ok_or(ExecutionError::UnresolvedBinding(resolution.atom))?;
        self.realm.initialize_lexical(slot, value)
    }

    fn environment_at_depth(&mut self, depth: u32) -> Result<GcRef<Environment>, ExecutionError> {
        let mut environment = self
            .fiber
            .frames
            .last()
            .and_then(|frame| frame.environment)
            .ok_or(ExecutionError::MissingEnvironment)?;
        for _ in 0..depth {
            environment = self.heap.with_running_scope(|scope| {
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_reference(environment, self.types.environment)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .parent
                        .ok_or(ExecutionError::MissingEnvironment)
                })
            })?;
        }
        Ok(environment)
    }

    fn load_environment(&mut self, depth: u32, slot: u32) -> Result<Value, ExecutionError> {
        let environment = self.environment_at_depth(depth)?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .slots
                    .get(slot as usize)
                    .copied()
                    .ok_or(ExecutionError::InvalidEnvironmentSlot { depth, slot })
            })
        })
    }

    /// Mutates one environment slot and records an old-to-young edge when the value is managed.
    fn store_environment(
        &mut self,
        depth: u32,
        slot: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let environment = self.environment_at_depth(depth)?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                let environment = no_gc
                    .borrow_reference_mut(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let target = environment
                    .slots
                    .get_mut(slot as usize)
                    .ok_or(ExecutionError::InvalidEnvironmentSlot { depth, slot })?;
                *target = value;
                Ok::<(), ExecutionError>(())
            })
        })?;
        if let Some(target) = value.as_heap_ref() {
            self.heap
                .write_barrier(environment.raw(), target)
                .map_err(ExecutionError::HeapReference)?;
        }
        Ok(())
    }

    /// Allocates the current activation's exact captured-slot backing after its frame is rooted.
    fn allocate_current_environment(&mut self, slot_count: u32) -> Result<(), ExecutionError> {
        if slot_count == 0 {
            return Ok(());
        }
        let slot_count = usize::try_from(slot_count)
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(slot_count)
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        slots.resize(slot_count, Value::from_immediate(Immediate::Undefined));
        let parent = self.fiber.frames.last().and_then(|frame| frame.environment);
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let environment = self
            .heap
            .try_allocate_external_with_gc(
                self.types.environment,
                0,
                Environment {
                    parent,
                    slots: slots.into_boxed_slice(),
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.fiber
            .frames
            .last_mut()
            .expect("environment allocation retains its frame")
            .environment = Some(environment);
        Ok(())
    }

    fn enter(&mut self, code: CodeId, function_id: FunctionId) -> Result<(), ExecutionError> {
        let (layout, kind, strictness) = {
            let function = self
                .loaded_code(code)?
                .module
                .function(function_id)
                .ok_or(ExecutionError::MissingEntryFunction(function_id))?;
            (function.layout(), function.kind(), function.strictness())
        };
        let register_count = usize::try_from(layout.register_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(layout.register_count))?;
        if layout.register_count > self.stack_limits.max_registers {
            return Err(ExecutionError::RegisterStackLimit {
                limit: self.stack_limits.max_registers,
                requested: layout.register_count,
            });
        }
        if self.stack_limits.max_frames == 0 {
            return Err(ExecutionError::CallStackLimit { limit: 0 });
        }
        self.fiber.frames.clear();
        self.fiber.registers.clear();
        self.fiber.handlers.clear();
        self.fiber.completions.clear();
        self.fiber.pending_exception = None;
        self.reserve_entry_state(layout, register_count)?;
        self.fiber
            .registers
            .resize(register_count, Value::from_immediate(Immediate::Undefined));
        self.fiber.frames.push(Frame {
            code,
            function: function_id,
            pc: WordOffset::new(0),
            base: 0,
            environment: None,
            return_register: None,
            return_continuation: false,
            this_value: if matches!(kind, FunctionKind::Module) {
                Value::from_immediate(Immediate::Undefined)
            } else {
                self.realm
                    .global_object
                    .expect("realm initialization publishes a global object")
            },
            new_target: Value::from_immediate(Immediate::Undefined),
            strictness,
            argument_base: 0,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            handler_base: 0,
            completion_base: 0,
            construct_receiver: None,
            call_site: None,
        });
        self.allocate_current_environment(layout.environment_slot_count)
    }

    /// Allocates a real GC-managed callable instead of encoding FunctionId in a reserved Value tag.
    #[inline(never)]
    fn create_closure(
        &mut self,
        code: CodeId,
        base: u32,
        destination: u32,
        function: FunctionId,
    ) -> Result<(), ExecutionError> {
        self.loaded_code(code)?
            .module
            .function(function)
            .ok_or(ExecutionError::MissingEntryFunction(function))?;
        let environment = self.fiber.frames.last().and_then(|frame| frame.environment);
        let internal_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before bytecode execution");
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let closure = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Bytecode {
                        code,
                        function,
                        environment,
                    },
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
        self.write(base, destination, Value::from_heap_ref(closure.raw()))
    }

    /// Validates the constructor before allocation, creates its receiver, and pushes one JS frame.
    #[inline(never)]
    fn construct(
        &mut self,
        caller_base: u32,
        destination: u32,
        callee_register: u32,
        argument_count: u32,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        let constructor = self.read(caller_base, callee_register)?;
        let mut site = CallSite {
            caller_base,
            destination,
            callee: constructor,
            argument_base: caller_base
                .checked_add(callee_register)
                .and_then(|base| base.checked_add(1))
                .ok_or(ExecutionError::RegisterWindowTooLarge(argument_count))?,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: constructor,
            construct_receiver: None,
            call_site,
        };
        loop {
            let callable = self
                .resolve_function_object(site.callee)
                .map_err(|_| ExecutionError::NonConstructor(site.callee))?;
            match callable.executable {
                FunctionExecutable::Bound(data) => {
                    if site.argument_prefix.is_some() {
                        return Err(ExecutionError::BoundArgumentCountOverflow);
                    }
                    let bound = self.bound_function_snapshot(data)?;
                    site.argument_count = site
                        .argument_count
                        .checked_add(bound.argument_count)
                        .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                    site.argument_prefix = Some(data);
                    site.argument_prefix_count = bound.argument_count;
                    let (target, new_target) =
                        self.resolve_bound_construct_target(site.callee, site.new_target)?;
                    debug_assert_eq!(target, bound.call_target);
                    site.callee = target;
                    site.new_target = new_target;
                }
                FunctionExecutable::Native(NativeFunction::NumberConstructor) => {
                    return self.dispatch_conversion_native(
                        NativeFunction::NumberConstructor,
                        &site,
                        true,
                    );
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::StringConstructor
                    | NativeFunction::BooleanConstructor),
                ) => {
                    let value = self.primitive_constructor_value(native, &site)?;
                    return self.write(caller_base, destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectConstructor) => {
                    let object = self.create_object_from_site(&site)?;
                    return self.write(caller_base, destination, object);
                }
                FunctionExecutable::Native(NativeFunction::ErrorConstructor(kind)) => {
                    let message = self.call_argument(&site, 0)?;
                    let error = self.create_native_error(kind, message)?;
                    return self.write(caller_base, destination, error);
                }
                FunctionExecutable::Native(NativeFunction::ArrayConstructor) => {
                    let array = self.create_array_from_site(&site)?;
                    return self.write(caller_base, destination, array);
                }
                FunctionExecutable::Native(NativeFunction::FunctionConstructor) => {
                    return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
                }
                FunctionExecutable::Bytecode { .. } => break,
                FunctionExecutable::Native(_) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
            }
        }
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(site.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or(Value::from_immediate(Immediate::Null));
        let receiver = self.create_ordinary_object_with_prototype(prototype)?;
        site.this_value = receiver;
        site.construct_receiver = Some(receiver);
        self.call(site)
    }

    /// Applies each bound exotic's observable newTarget substitution without merging arguments.
    fn resolve_bound_construct_target(
        &mut self,
        mut target: Value,
        mut new_target: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        loop {
            let function = self.resolve_function_object(target)?;
            let FunctionExecutable::Bound(data) = function.executable else {
                return Ok((target, new_target));
            };
            let bound = self.bound_function_snapshot(data)?;
            if new_target == target {
                new_target = bound.bound_target;
            }
            target = bound.bound_target;
        }
    }

    /// Resolves native forwarding iteratively, then pushes one exact bytecode frame when required.
    #[inline(never)]
    fn call(&mut self, mut site: CallSite) -> Result<(), ExecutionError> {
        loop {
            let function_object = self.resolve_function_object(site.callee)?;
            match function_object.executable {
                FunctionExecutable::Bound(data) => {
                    if site.argument_prefix.is_some() {
                        return Err(ExecutionError::BoundArgumentCountOverflow);
                    }
                    let bound = self.bound_function_snapshot(data)?;
                    site.argument_count = site
                        .argument_count
                        .checked_add(bound.argument_count)
                        .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                    site.argument_prefix = Some(data);
                    site.argument_prefix_count = bound.argument_count;
                    site.callee = bound.call_target;
                    site.this_value = bound.bound_this;
                }
                FunctionExecutable::Bytecode {
                    code,
                    function,
                    environment,
                } => {
                    let (layout, strictness) = {
                        let function_template =
                            self.loaded_code(code)?
                                .module
                                .function(function)
                                .ok_or(ExecutionError::MissingEntryFunction(function))?;
                        (function_template.layout(), function_template.strictness())
                    };
                    return self.push_call_frame(
                        ResolvedCallTarget {
                            code,
                            function,
                            environment,
                            layout,
                            strictness,
                        },
                        site,
                    );
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototype) => {
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::NumberIsNaN
                    | NativeFunction::NumberIsFinite
                    | NativeFunction::NumberIsInteger
                    | NativeFunction::NumberIsSafeInteger),
                ) => {
                    let argument = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = numeric_value(argument).is_some_and(|number| match native {
                        NativeFunction::NumberIsNaN => number.is_nan(),
                        NativeFunction::NumberIsFinite => number.is_finite(),
                        NativeFunction::NumberIsInteger => {
                            number.is_finite() && number.fract() == 0.0
                        }
                        NativeFunction::NumberIsSafeInteger => {
                            number.is_finite()
                                && number.fract() == 0.0
                                && number.abs() <= crate::array::MAX_SAFE_INTEGER as f64
                        }
                        _ => unreachable!("numeric predicate dispatch is exhaustive"),
                    });
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::NumberValueOf) => {
                    let value = self.this_number_value(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::StringConstructor
                    | NativeFunction::NumberToExponential
                    | NativeFunction::NumberToFixed
                    | NativeFunction::NumberToPrecision
                    | NativeFunction::NumberToString
                    | NativeFunction::NumberConstructor),
                ) => return self.dispatch_conversion_native(native, &site, false),
                FunctionExecutable::Native(NativeFunction::ObjectConstructor) => {
                    let object = self.create_object_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, object);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::SymbolConstructor
                    | NativeFunction::BooleanConstructor),
                ) => {
                    let value = self.primitive_constructor_value(native, &site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectDefineProperty) => {
                    let object = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let key = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let descriptor = self
                        .call_argument(&site, 2)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let key = self.property_key_atom_or_undefined(key)?;
                    let descriptor = self.parse_data_property_descriptor(descriptor)?;
                    self.define_data_property(object, key, descriptor)?;
                    return self.write(site.caller_base, site.destination, object);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyDescriptor) => {
                    let object = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    if matches!(
                        object.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) {
                        return Err(ExecutionError::NotObject(object));
                    }
                    let key = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let key = self.property_key_atom_or_undefined(key)?;
                    let property = if self.is_object_value(object) {
                        self.own_data_property_with_attributes(object, key)?
                    } else {
                        None
                    };
                    let Some((value, attributes)) = property else {
                        return self.write(
                            site.caller_base,
                            site.destination,
                            Value::from_immediate(Immediate::Undefined),
                        );
                    };
                    self.write(site.caller_base, site.destination, value)?;
                    let result = self.create_ordinary_object()?;
                    self.write(site.caller_base, site.destination, result)?;
                    self.materialize_data_property_descriptor(result, value, attributes)?;
                    return Ok(());
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyNames) => {
                    let result = self.object_get_own_property_names(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ObjectHasOwnProperty) => {
                    let key = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let key = self.property_key_atom_or_undefined(key)?;
                    let result = self.has_own_data_property(site.this_value, key)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectPropertyIsEnumerable) => {
                    let key = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let key = self.property_key_atom_or_undefined(key)?;
                    let enumerable = self
                        .own_data_property_with_attributes(site.this_value, key)?
                        .is_some_and(|(_, attributes)| attributes.enumerable());
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if enumerable {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectHasOwn) => {
                    let object = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    if matches!(
                        object.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) {
                        return Err(ExecutionError::NotObject(object));
                    }
                    let key = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let key = self.property_key_atom_or_undefined(key)?;
                    let result = self.has_own_data_property(object, key)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectIs) => {
                    let left = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let right = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = self.same_value(left, right)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetPrototypeOf) => {
                    let object = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let prototype = self.object_prototype_of(object)?;
                    return self.write(site.caller_base, site.destination, prototype);
                }
                FunctionExecutable::Native(NativeFunction::ObjectCreate) => {
                    let prototype = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    if prototype.as_immediate() != Some(Immediate::Null)
                        && !self.is_object_value(prototype)
                    {
                        return Err(ExecutionError::NotObject(prototype));
                    }
                    let object = self.create_ordinary_object_with_prototype(prototype)?;
                    self.write(site.caller_base, site.destination, object)?;
                    if let Some(descriptors) = self.call_argument(&site, 1)?
                        && descriptors.as_immediate() != Some(Immediate::Undefined)
                    {
                        self.define_ordinary_properties(object, descriptors)?;
                    }
                    return Ok(());
                }
                FunctionExecutable::Native(NativeFunction::ObjectIsPrototypeOf) => {
                    let value = self.call_argument(&site, 0)?;
                    let result = self.is_prototype_of(site.this_value, value)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectIsExtensible) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = if self.is_object_value(value) {
                        self.object_snapshot(value)?.1.extensible
                    } else {
                        false
                    };
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectPreventExtensions) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    if self.is_object_value(value) {
                        let (receiver, _) = self.object_snapshot(value)?;
                        self.set_object_extensible(receiver, false)?;
                    }
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectToString) => {
                    let string = self.object_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, string);
                }
                FunctionExecutable::Native(NativeFunction::ObjectAssign) => {
                    let target = self.object_assign(&site)?;
                    return self.write(site.caller_base, site.destination, target);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::ObjectKeys
                    | NativeFunction::ObjectValues
                    | NativeFunction::ObjectEntries),
                ) => {
                    let result = self.object_enumeration(&site, native)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototypeCall) => {
                    let this_argument = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    site.callee = site.this_value;
                    site.this_value = this_argument;
                    if site.argument_count != 0 {
                        if site.argument_prefix_count != 0 {
                            site.argument_prefix_offset += 1;
                            site.argument_prefix_count -= 1;
                        } else {
                            site.argument_base = site.argument_base.checked_add(1).ok_or(
                                ExecutionError::RegisterWindowTooLarge(site.argument_count),
                            )?;
                        }
                        site.argument_count -= 1;
                    }
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototypeBind) => {
                    let bound = self.create_bound_function(&site)?;
                    return self.write(site.caller_base, site.destination, bound);
                }
                FunctionExecutable::Native(NativeFunction::FunctionConstructor) => {
                    return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
                }
                FunctionExecutable::Native(NativeFunction::ErrorConstructor(kind)) => {
                    let message = self.call_argument(&site, 0)?;
                    let error = self.create_native_error(kind, message)?;
                    return self.write(site.caller_base, site.destination, error);
                }
                FunctionExecutable::Native(NativeFunction::ArrayConstructor) => {
                    let array = self.create_array_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, array);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIsArray) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = self.is_array_value(value)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ArrayConcat) => {
                    let array = self.array_concat(&site)?;
                    return self.write(site.caller_base, site.destination, array);
                }
                FunctionExecutable::Native(NativeFunction::ArrayPush) => {
                    let length = self.array_push(&site)?;
                    return self.write(site.caller_base, site.destination, length);
                }
                FunctionExecutable::Native(NativeFunction::ArrayJoin) => {
                    let value = self.array_join(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayAt) => {
                    let value = self.array_at(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIndexOf) => {
                    let value = self.array_search(&site, false)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIncludes) => {
                    let value = self.array_search(&site, true)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayPop) => {
                    let value = self.array_pop(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArraySlice) => {
                    let value = self.array_slice(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayShift) => {
                    let value = self.array_shift(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayUnshift) => {
                    let value = self.array_unshift(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayReverse) => {
                    let value = self.array_reverse(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFill) => {
                    let value = self.array_fill(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayLastIndexOf) => {
                    let value = self.array_last_index_of(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayCopyWithin) => {
                    let value = self.array_copy_within(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFlat) => {
                    let value = self.array_flat(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArraySort) => {
                    let value = self.array_sort(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayToString) => {
                    let value = self.array_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MathPow) => {
                    let left = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let right = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let left = numeric_value(self.convert_to_number(left)?)
                        .ok_or(ExecutionError::UnsupportedNumberConversion(left))?;
                    let right = numeric_value(self.convert_to_number(right)?)
                        .ok_or(ExecutionError::UnsupportedNumberConversion(right))?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_f64(left.powf(right)),
                    );
                }
            }
        }
    }

    #[inline(always)]
    fn resolve_function_object(&mut self, callee: Value) -> Result<FunctionObject, ExecutionError> {
        let raw = callee
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(callee))?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_raw_reference(raw, self.types.function)
                    .copied()
                    .map_err(|_| ExecutionError::NonCallable(callee))
            })
        })
    }

    #[inline(always)]
    fn call_argument(
        &mut self,
        site: &CallSite,
        index: u32,
    ) -> Result<Option<Value>, ExecutionError> {
        if index >= site.argument_count {
            return Ok(None);
        }
        if index < site.argument_prefix_count {
            let data = site
                .argument_prefix
                .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
            let index = site
                .argument_prefix_offset
                .checked_add(index)
                .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
            return self.bound_function_argument(data, index).map(Some);
        }
        let suffix_index = index - site.argument_prefix_count;
        let absolute = site
            .argument_base
            .checked_add(suffix_index)
            .ok_or(ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        self.fiber
            .registers
            .get(absolute as usize)
            .copied()
            .map(Some)
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(
                suffix_index,
            )))
    }

    /// Reserves the callee state before mutation, then copies the supplied positional arguments.
    fn push_call_frame(
        &mut self,
        target: ResolvedCallTarget,
        site: CallSite,
    ) -> Result<(), ExecutionError> {
        if self.fiber.frames.len() >= self.stack_limits.max_frames as usize {
            return Err(ExecutionError::CallStackLimit {
                limit: self.stack_limits.max_frames,
            });
        }
        let register_count = target.layout.register_count;
        let callee_base = u32::try_from(self.fiber.registers.len())
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(register_count))?;
        let requested = callee_base
            .checked_add(register_count)
            .ok_or(ExecutionError::RegisterWindowTooLarge(register_count))?;
        if requested > self.stack_limits.max_registers {
            return Err(ExecutionError::RegisterStackLimit {
                limit: self.stack_limits.max_registers,
                requested,
            });
        }
        let additional = register_count as usize;
        self.fiber
            .frames
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        self.fiber
            .registers
            .try_reserve_exact(additional)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        self.fiber.registers.resize(
            requested as usize,
            Value::from_immediate(Immediate::Undefined),
        );
        let copied_arguments = site.argument_count.min(target.layout.argument_count);
        for index in 0..copied_arguments {
            let value = self
                .call_argument(&site, index)?
                .expect("copied argument index is within total count");
            self.write(callee_base, index, value)?;
        }
        let this_value = self.bind_ordinary_this(target.strictness, site.this_value);
        self.fiber.frames.push(Frame {
            code: target.code,
            function: target.function,
            pc: WordOffset::new(0),
            base: callee_base,
            environment: target.environment,
            return_register: Some(RegisterId::new(site.destination)),
            return_continuation: false,
            this_value,
            new_target: site.new_target,
            construct_receiver: site.construct_receiver,
            strictness: target.strictness,
            argument_base: site.argument_base,
            argument_prefix: site.argument_prefix,
            argument_prefix_offset: site.argument_prefix_offset,
            argument_prefix_count: site.argument_prefix_count,
            argument_count: site.argument_count,
            handler_base: self.fiber.handlers.len() as u32,
            completion_base: self.fiber.completions.len() as u32,
            call_site: Some(site.call_site),
        });
        if let Err(error) = self.allocate_current_environment(target.layout.environment_slot_count)
        {
            self.fiber.frames.pop();
            self.fiber.registers.truncate(callee_base as usize);
            return Err(error);
        }
        Ok(())
    }

    #[inline(always)]
    fn bind_ordinary_this(&self, strictness: FunctionStrictness, this_argument: Value) -> Value {
        if strictness == FunctionStrictness::Strict
            || !matches!(
                this_argument.as_immediate(),
                Some(Immediate::Undefined | Immediate::Null)
            )
        {
            return this_argument;
        }
        self.realm
            .global_object
            .expect("realm initialization publishes a global object")
    }

    /// Selects top-level completion or the hot ordinary-callee frame return path.
    #[inline(always)]
    fn finish_return(&mut self, value: Value) -> Result<Option<RunOutcome>, ExecutionError> {
        if self.fiber.frames.len() == 1 {
            return Ok(Some(RunOutcome::Completed(value)));
        }
        self.return_from_callee(value)
    }

    /// Pops a non-entry frame and restores caller checkpoints on the ordinary call hot path.
    #[inline(always)]
    fn return_from_callee(&mut self, value: Value) -> Result<Option<RunOutcome>, ExecutionError> {
        let frame = self
            .fiber
            .frames
            .pop()
            .expect("callee return always has an active frame");
        let value = match frame.construct_receiver {
            Some(receiver) if !self.is_object_value(value) => receiver,
            _ => value,
        };
        self.fiber.registers.truncate(frame.base as usize);
        self.fiber.handlers.truncate(frame.handler_base as usize);
        let continuation = if frame.return_continuation {
            Some(self.pop_native_continuation()?)
        } else {
            None
        };
        self.fiber
            .completions
            .truncate(frame.completion_base as usize);
        if let Some(continuation) = continuation {
            return self.resume_native_continuation(continuation, value);
        }
        let destination = frame
            .return_register
            .expect("non-entry frames always retain a caller destination");
        let caller_base = self
            .fiber
            .frames
            .last()
            .expect("a callee return with a destination retains its caller")
            .base;
        self.write(caller_base, destination.index(), value)?;
        Ok(None)
    }

    /// Resumes native work after a callback return and maps language failures at the original call site.
    fn resume_native_continuation(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match self.advance_native_conversion(continuation, Some(value)) {
            Ok(()) => Ok(None),
            Err(error) => {
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                self.throw_native_error(kind, continuation.site.call_site)
            }
        }
    }

    /// Propagates a thrown value through explicit frames until an immutable handler range matches.
    #[cold]
    #[inline(never)]
    fn throw_value(
        &mut self,
        value: Value,
        mut instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let frame = *self
                .fiber
                .frames
                .last()
                .expect("throw dispatch always has an active frame");
            if let Some(handler) = self.find_exception_handler(frame, instruction_offset)? {
                if handler.kind != HandlerKind::Catch {
                    return Err(ExecutionError::UnsupportedExceptionHandler(handler.kind));
                }
                let active = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("matched handler retains its frame");
                active.pc = handler.handler;
                self.fiber.handlers.truncate(frame.handler_base as usize);
                self.fiber
                    .completions
                    .truncate(frame.completion_base as usize);
                self.fiber.pending_exception = Some(value);
                return Ok(None);
            }
            if self.fiber.frames.len() == 1 {
                return Ok(self.unhandled_throw(value));
            }
            let frame = self
                .fiber
                .frames
                .pop()
                .expect("non-entry throw retains a callee frame");
            self.fiber.registers.truncate(frame.base as usize);
            self.fiber.handlers.truncate(frame.handler_base as usize);
            self.fiber
                .completions
                .truncate(frame.completion_base as usize);
            instruction_offset = frame
                .call_site
                .expect("every non-entry frame records its caller call-site");
        }
    }

    /// Selects the innermost half-open handler range for one verified function offset.
    #[inline]
    fn find_exception_handler(
        &self,
        frame: Frame,
        instruction_offset: WordOffset,
    ) -> Result<Option<HandlerEntry>, ExecutionError> {
        let function = self
            .loaded_code(frame.code)?
            .module
            .function(frame.function)
            .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
        Ok(function.handlers().iter().rev().copied().find(|handler| {
            handler.protected_start.index() <= instruction_offset.index()
                && instruction_offset.index() < handler.protected_end.index()
        }))
    }

    /// Preserves the active fiber as the root owner until the host observes the unhandled value.
    #[cold]
    #[inline(never)]
    fn unhandled_throw(&mut self, value: Value) -> Option<RunOutcome> {
        Some(RunOutcome::Thrown(value))
    }

    /// Reserves the exact entry-function execution windows before any opcode can push into them.
    ///
    /// Calls will extend these windows from the callee's verified metadata in a later VM package.
    /// Until then, reserving the handler and completion depths here proves the no-reallocation
    /// contract without conflating bytecode decoding with collection growth policy.
    fn reserve_entry_state(
        &mut self,
        layout: tachyon_bytecode::FunctionLayout,
        register_count: usize,
    ) -> Result<(), ExecutionError> {
        let handler_depth = usize::try_from(layout.max_handler_depth)
            .map_err(|_| ExecutionError::HandlerStackTooLarge(layout.max_handler_depth))?;
        let completion_depth = usize::try_from(layout.max_completion_depth)
            .map_err(|_| ExecutionError::CompletionStackTooLarge(layout.max_completion_depth))?;
        self.fiber
            .frames
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        self.fiber
            .registers
            .try_reserve_exact(register_count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        self.fiber
            .handlers
            .try_reserve_exact(handler_depth)
            .map_err(|_| ExecutionError::HandlerAllocationFailed)?;
        self.fiber
            .completions
            .try_reserve_exact(completion_depth)
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
        Ok(())
    }

    #[inline(always)]
    fn read(&self, base: u32, register: u32) -> Result<Value, ExecutionError> {
        self.fiber
            .registers
            .get(base as usize + register as usize)
            .copied()
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(register)))
    }

    #[inline(always)]
    fn write(&mut self, base: u32, register: u32, value: Value) -> Result<(), ExecutionError> {
        let slot = self
            .fiber
            .registers
            .get_mut(base as usize + register as usize)
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(register)))?;
        *slot = value;
        Ok(())
    }

    #[inline(always)]
    fn set_pc(&mut self, pc: WordOffset) {
        self.fiber
            .frames
            .last_mut()
            .expect("frame remains active while jumping")
            .pc = pc;
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

/// Applies one numeric-only binary opcode after both operands have completed ToNumber.
#[inline(always)]
fn numeric_binary_operation(opcode: Opcode, left: Value, right: Value) -> Value {
    match opcode {
        Opcode::Sub | Opcode::Mul | Opcode::Div => numeric_binary(opcode, left, right),
        Opcode::BitwiseAnd | Opcode::BitwiseOr | Opcode::BitwiseXor => {
            numeric_bitwise_binary(opcode, left, right)
        }
        Opcode::ShiftLeft | Opcode::ShiftRight | Opcode::ShiftRightUnsigned => {
            numeric_shift(opcode, left, right)
        }
        Opcode::Remainder | Opcode::Exponentiate => numeric_remainder_or_power(opcode, left, right),
        _ => unreachable!("numeric binary continuation received a non-numeric opcode"),
    }
}

#[inline(always)]
fn numeric_binary(opcode: Opcode, left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_i32(), right.as_i32()) {
        let integer = match opcode {
            Opcode::Add => left.checked_add(right),
            Opcode::Sub => left.checked_sub(right),
            Opcode::Mul => left.checked_mul(right),
            Opcode::Div if right != 0 && left % right == 0 => left.checked_div(right),
            _ => None,
        };
        if let Some(integer) = integer {
            return Value::from_i32(integer);
        }
    }
    let left_number = left
        .as_i32()
        .map_or_else(|| left.as_f64().unwrap_or(f64::NAN), f64::from);
    let right_number = right
        .as_i32()
        .map_or_else(|| right.as_f64().unwrap_or(f64::NAN), f64::from);
    Value::from_f64(match opcode {
        Opcode::Add => left_number + right_number,
        Opcode::Sub => left_number - right_number,
        Opcode::Mul => left_number * right_number,
        Opcode::Div => left_number / right_number,
        _ => unreachable!("numeric binary dispatch only supplies arithmetic opcodes"),
    })
}

#[inline(always)]
fn numeric_negate(value: Value) -> Value {
    if let Some(integer) = value.as_i32() {
        if integer == 0 {
            return Value::from_f64(-0.0);
        }
        return integer
            .checked_neg()
            .map_or_else(|| Value::from_f64(-f64::from(integer)), Value::from_i32);
    }
    if let Some(number) = value.as_f64() {
        return Value::from_f64(-number);
    }
    Value::from_f64(match value.as_immediate() {
        Some(Immediate::Null | Immediate::False) => -0.0,
        Some(Immediate::True) => -1.0,
        _ => f64::NAN,
    })
}

/// Applies ECMAScript ToInt32 before complementing, including modulo-2^32 wrapping.
#[inline(always)]
fn numeric_bitwise_not(value: Value) -> Value {
    let number = value
        .as_i32()
        .map(f64::from)
        .or_else(|| value.as_f64())
        .unwrap_or(f64::NAN);
    let integer = if number.is_finite() && number != 0.0 {
        let modulo = number.trunc().rem_euclid(4_294_967_296.0);
        if modulo >= 2_147_483_648.0 {
            modulo - 4_294_967_296.0
        } else {
            modulo
        }
    } else {
        0.0
    };
    Value::from_i32(!(integer as i32))
}

/// Applies ToInt32 to both operands and performs one supported bitwise operation.
#[inline(always)]
fn numeric_bitwise_binary(opcode: Opcode, left: Value, right: Value) -> Value {
    let left = numeric_bitwise_int32(left);
    let right = numeric_bitwise_int32(right);
    let result = match opcode {
        Opcode::BitwiseAnd => left & right,
        Opcode::BitwiseOr => left | right,
        Opcode::BitwiseXor => left ^ right,
        _ => unreachable!("bitwise binary dispatch only supplies bitwise opcodes"),
    };
    Value::from_i32(result)
}

#[inline(always)]
fn numeric_bitwise_int32(value: Value) -> i32 {
    value.as_i32().unwrap_or_else(|| {
        let number = value.as_f64().unwrap_or(f64::NAN);
        if !number.is_finite() || number == 0.0 {
            return 0;
        }
        let modulo = number.trunc().rem_euclid(4_294_967_296.0);
        let signed = if modulo >= 2_147_483_648.0 {
            modulo - 4_294_967_296.0
        } else {
            modulo
        };
        signed as i32
    })
}

/// Applies ECMAScript shift-count masking and signed/unsigned left operand conversion.
#[inline(always)]
fn numeric_shift(opcode: Opcode, left: Value, right: Value) -> Value {
    let left_number = left
        .as_i32()
        .map(f64::from)
        .or_else(|| left.as_f64())
        .unwrap_or(f64::NAN);
    let right_number = right
        .as_i32()
        .map(f64::from)
        .or_else(|| right.as_f64())
        .unwrap_or(f64::NAN);
    let shift = numeric_bitwise_uint32(right_number) & 31;
    match opcode {
        Opcode::ShiftLeft => Value::from_i32(numeric_bitwise_int32(left) << shift),
        Opcode::ShiftRight => Value::from_i32(numeric_bitwise_int32(left) >> shift),
        Opcode::ShiftRightUnsigned => {
            Value::from_f64(f64::from(numeric_bitwise_uint32(left_number) >> shift))
        }
        _ => unreachable!("shift dispatch only supplies shift opcodes"),
    }
}

/// Executes `%` and `**` after both operands have crossed the numeric conversion boundary.
#[inline(always)]
fn numeric_remainder_or_power(opcode: Opcode, left: Value, right: Value) -> Value {
    let left = left
        .as_i32()
        .map(f64::from)
        .or_else(|| left.as_f64())
        .unwrap_or(f64::NAN);
    let right = right
        .as_i32()
        .map(f64::from)
        .or_else(|| right.as_f64())
        .unwrap_or(f64::NAN);
    let result = match opcode {
        Opcode::Remainder => left % right,
        Opcode::Exponentiate => left.powf(right),
        _ => unreachable!("arithmetic dispatch only supplies remainder or exponentiation"),
    };
    Value::from_f64(result)
}

/// Compares converted numeric operands while preserving false results for NaN.
#[inline(always)]
fn numeric_relational(opcode: Opcode, left: Value, right: Value) -> Value {
    let left = left
        .as_i32()
        .map(f64::from)
        .or_else(|| left.as_f64())
        .unwrap_or(f64::NAN);
    let right = right
        .as_i32()
        .map(f64::from)
        .or_else(|| right.as_f64())
        .unwrap_or(f64::NAN);
    let result = match opcode {
        Opcode::GreaterThan => left > right,
        Opcode::LessEqual => left <= right,
        Opcode::GreaterEqual => left >= right,
        _ => unreachable!("relational dispatch only supplies relational opcodes"),
    };
    Value::from_immediate(if result {
        Immediate::True
    } else {
        Immediate::False
    })
}

#[inline(always)]
fn numeric_bitwise_uint32(number: f64) -> u32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    number.trunc().rem_euclid(4_294_967_296.0) as u32
}

#[inline(always)]
fn numeric_value(value: Value) -> Option<f64> {
    value.as_i32().map(f64::from).or_else(|| value.as_f64())
}

#[inline(always)]
fn safe_integer_value(value: u64) -> Value {
    i32::try_from(value)
        .map(Value::from_i32)
        .unwrap_or_else(|_| Value::from_f64(value as f64))
}

/// Parses ECMAScript numeric string forms after the string has been detached from the heap.
fn parse_number_code_units(units: &[u16]) -> f64 {
    let text = String::from_utf16_lossy(units);
    let text = text.trim_matches(is_ecmascript_whitespace);
    if text.is_empty() {
        return 0.0;
    }
    let (radix, digits) = if let Some(digits) = text.strip_prefix("0x") {
        (16, digits)
    } else if let Some(digits) = text.strip_prefix("0X") {
        (16, digits)
    } else if let Some(digits) = text.strip_prefix("0b") {
        (2, digits)
    } else if let Some(digits) = text.strip_prefix("0B") {
        (2, digits)
    } else if let Some(digits) = text.strip_prefix("0o") {
        (8, digits)
    } else if let Some(digits) = text.strip_prefix("0O") {
        (8, digits)
    } else {
        return text.parse::<f64>().unwrap_or(f64::NAN);
    };
    if digits.is_empty() {
        return f64::NAN;
    }
    u64::from_str_radix(digits, radix)
        .map(|value| value as f64)
        .unwrap_or(f64::NAN)
}

#[inline]
fn is_ecmascript_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

#[inline(always)]
fn numeric_less_than(left: Value, right: Value) -> bool {
    match (left.as_i32(), right.as_i32()) {
        (Some(left), Some(right)) => left < right,
        _ => match (numeric_value(left), numeric_value(right)) {
            (Some(left), Some(right)) => left < right,
            _ => false,
        },
    }
}

#[inline(always)]
fn is_non_string_truthy(value: Value) -> bool {
    if let Some(integer) = value.as_i32() {
        return integer != 0;
    }
    if let Some(number) = value.as_f64() {
        return number != 0.0 && !number.is_nan();
    }
    !matches!(
        value.as_immediate(),
        Some(Immediate::Undefined | Immediate::Null | Immediate::False)
    )
}

#[inline(always)]
fn is_nullish(value: Value) -> bool {
    matches!(
        value.as_immediate(),
        Some(Immediate::Undefined | Immediate::Null)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tachyon_bytecode::{
        BindingLocation, BindingPlanEntry, Bytecode, BytecodeBuilder, BytecodeConstant,
        CompiledFunctionTemplate, CompiledModule, FunctionId, FunctionKind, FunctionLayout,
        FunctionMetadata, SourceSpan, encode_instruction,
    };
    use tachyon_gc::{ForcedCollectionMode, GcRef, HeapLimit, SPAN_SIZE_BYTES, Tracer};
    use tachyon_value::RawHeapRef;

    use super::*;

    fn test_isolate() -> Isolate {
        Isolate::new(IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(8 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ))
        .expect("test isolate descriptors register")
    }

    fn arithmetic_module() -> CompiledModule {
        binary_module(Opcode::Add, "1 + 2")
    }

    fn less_than_module() -> CompiledModule {
        binary_module(Opcode::LessThan, "1 < 2")
    }

    /// Builds a closure whose empty activation inherits and mutates the entry environment.
    fn captured_environment_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::default();
        entry.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
        entry
            .emit(Opcode::StoreEnvironment, &[0, 0, 0], span)
            .unwrap();
        entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
        entry.emit(Opcode::Call, &[2, 1, 0], span).unwrap();
        entry.emit(Opcode::Call, &[3, 1, 0], span).unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut closure = BytecodeBuilder::default();
        closure
            .emit(Opcode::LoadEnvironment, &[0, 0, 0], span)
            .unwrap();
        closure.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
        closure.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
        closure
            .emit(Opcode::StoreEnvironment, &[2, 0, 0], span)
            .unwrap();
        closure.emit(Opcode::Return, &[2], span).unwrap();
        let (closure_bytecode, closure_source_map, closure_registers) = closure.finish().unwrap();
        let binding_plan: Arc<[BindingPlanEntry]> = Arc::from([BindingPlanEntry {
            name: Arc::from("value"),
            location: BindingLocation::Environment { depth: 0, slot: 0 },
            mutable: true,
        }]);
        CompiledModule::new(
            Arc::from("captured environment"),
            vec![],
            vec![],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Script,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            environment_slot_count: 1,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        binding_plan: binding_plan.clone(),
                        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    closure_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Ordinary,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: closure_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: closure_source_map,
                        binding_plan,
                        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Compares the canonical typeof number value with an independently loaded string literal.
    fn typeof_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(5, 0);
        builder.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
        builder.emit(Opcode::Typeof, &[1, 0], span).unwrap();
        builder.emit(Opcode::LoadConstant, &[2, 0], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[3, 1, 2], span).unwrap();
        builder.emit(Opcode::Return, &[3], span).unwrap();
        single_function_module(
            "typeof number",
            vec![BytecodeConstant::string_from_utf16(
                "number".encode_utf16().collect(),
            )],
            builder,
        )
    }

    /// Loads two distinct strings so forced collection runs while the pending cache owns a root.
    fn string_constant_root_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(4, 0);
        builder.emit(Opcode::LoadConstant, &[0, 0], span).unwrap();
        builder.emit(Opcode::LoadConstant, &[1, 1], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[2, 0, 1], span).unwrap();
        builder.emit(Opcode::Return, &[2], span).unwrap();
        single_function_module(
            "rooted strings",
            vec![
                BytecodeConstant::string_from_utf16("left".encode_utf16().collect()),
                BytecodeConstant::string_from_utf16("right".encode_utf16().collect()),
            ],
            builder,
        )
    }

    /// Declares one global twice around a write so dispatch tests prove redeclaration is a no-op.
    fn scoped_var_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 0);
        builder.emit(Opcode::DeclareScope, &[0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
        builder
            .emit(Opcode::StoreResolvedScope, &[0, 0], span)
            .unwrap();
        builder.emit(Opcode::DeclareScope, &[0], span).unwrap();
        builder.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        CompiledModule::new(
            Arc::from("var answer = 7; var answer; answer;"),
            Vec::new(),
            vec![Arc::from("answer")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count,
                        ..FunctionLayout::default()
                    },
                    source_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Exercises declarative global declaration, one-time initialization, and lexical-first load.
    fn global_lexical_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(5, 0);
        builder
            .emit(Opcode::DeclareGlobalLexical, &[0, 1], span)
            .unwrap();
        builder.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
        builder
            .emit(Opcode::InitializeGlobalLexical, &[0, 0], span)
            .unwrap();
        builder.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        CompiledModule::new(
            Arc::from("let answer = 42; answer;"),
            Vec::new(),
            vec![Arc::from("answer")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count,
                        ..FunctionLayout::default()
                    },
                    source_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Freezes one builder into a script module with caller-provided immutable constants.
    fn single_function_module(
        source: &'static str,
        constants: Vec<BytecodeConstant>,
        builder: BytecodeBuilder,
    ) -> CompiledModule {
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from(source),
            constants,
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a minimal verified binary-op fixture over the integer values one and two.
    fn binary_module(opcode: Opcode, source: &'static str) -> CompiledModule {
        let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 1]).unwrap();
        words.extend(encode_instruction(Opcode::LoadImmediate, &[1, 2]).unwrap());
        words.extend(encode_instruction(opcode, &[2, 0, 1]).unwrap());
        words.extend(encode_instruction(Opcode::Return, &[2]).unwrap());
        let metadata = FunctionMetadata::new(
            FunctionKind::Script,
            FunctionLayout {
                register_count: 3,
                ..FunctionLayout::default()
            },
        );
        CompiledModule::new(
            Arc::from(source),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                Bytecode::from_words(words),
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a call whose integer callee must become a language-level TypeError.
    fn non_callable_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::default();
        builder.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
        builder.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        single_function_module("1()", Vec::new(), builder)
    }

    /// Builds a zero-register callee so ReturnUndefined exercises ordinary frame unwinding.
    fn undefined_call_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::default();
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::Return, &[1], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::default();
        callee.emit(Opcode::ReturnUndefined, &[], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_layout = FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        };
        let callee_layout = FunctionLayout {
            register_count: callee_registers,
            ..FunctionLayout::default()
        };
        CompiledModule::new(
            Arc::from("function empty() {} empty();"),
            Vec::new(),
            Vec::new(),
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        source_map: entry_source_map,
                        ..FunctionMetadata::new(FunctionKind::Script, entry_layout)
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callee_bytecode,
                    FunctionMetadata {
                        source_map: callee_source_map,
                        ..FunctionMetadata::new(FunctionKind::Ordinary, callee_layout)
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds one non-capturing function call with a contiguous single-argument window.
    fn call_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 0 };
        let mut entry = BytecodeBuilder::default();
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[1, 40], span).unwrap();
        entry.emit(Opcode::Call, &[2, 0, 1], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callee = BytecodeBuilder::default();
        callee.emit(Opcode::LoadImmediate, &[1, 2], span).unwrap();
        callee.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
        callee.emit(Opcode::Return, &[2], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();

        CompiledModule::new(
            Arc::from("function addTwo(value) { return value + 2; } addTwo(40);"),
            vec![],
            vec![],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Script,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callee_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Ordinary,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: callee_registers,
                            argument_count: 1,
                            ..FunctionLayout::default()
                        },
                        source_map: callee_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a callee throw so batch tests cover abrupt exit after an explicit frame switch.
    fn throwing_call_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 0 };
        let mut entry = BytecodeBuilder::default();
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::Return, &[1], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callee = BytecodeBuilder::default();
        callee.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
        callee.emit(Opcode::Throw, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();

        CompiledModule::new(
            Arc::from("function fail() { throw 7; } fail();"),
            vec![],
            vec![],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Script,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callee_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Ordinary,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: callee_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: callee_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds separate publisher/caller modules to exercise CodeId changes inside one dispatch batch.
    fn cross_code_modules() -> (CompiledModule, CompiledModule) {
        let span = SourceSpan { start: 0, end: 0 };
        let mut publisher_entry = BytecodeBuilder::default();
        publisher_entry
            .emit(Opcode::CreateClosure, &[0, 1], span)
            .unwrap();
        publisher_entry
            .emit(Opcode::StoreScope, &[0, 0], span)
            .unwrap();
        publisher_entry.emit(Opcode::Return, &[0], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = publisher_entry.finish().unwrap();
        let mut published_function = BytecodeBuilder::default();
        published_function
            .emit(Opcode::LoadImmediate, &[0, 42], span)
            .unwrap();
        published_function.emit(Opcode::Return, &[0], span).unwrap();
        let (function_bytecode, function_source_map, function_registers) =
            published_function.finish().unwrap();
        let publisher = CompiledModule::new(
            Arc::from("publisher"),
            vec![],
            vec![Arc::from("answer")],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Script,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    function_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Ordinary,
                        strictness: FunctionStrictness::Sloppy,
                        layout: FunctionLayout {
                            register_count: function_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: function_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap();

        let mut caller_entry = BytecodeBuilder::default();
        caller_entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
        caller_entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        caller_entry.emit(Opcode::Return, &[1], span).unwrap();
        let (caller_bytecode, caller_source_map, caller_registers) = caller_entry.finish().unwrap();
        let caller = CompiledModule::new(
            Arc::from("caller"),
            vec![],
            vec![Arc::from("answer")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                caller_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Script,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: caller_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: caller_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                },
            )],
            FunctionId::new(0),
        )
        .unwrap();
        (publisher, caller)
    }

    fn assert_call_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &call_module(),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_captured_environment_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &captured_environment_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    fn assert_throw_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &throwing_call_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Thrown(value) if value.as_i32() == Some(7)));
    }

    fn assert_cross_code_batch<const N: usize>() {
        let (publisher, caller) = cross_code_modules();
        let mut isolate = test_isolate();
        isolate
            .execute_with_batch::<N>(
                &publisher,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        let outcome = isolate
            .execute_with_batch::<N>(
                &caller,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_property_batch<const N: usize>() {
        for module in [property_module(), function_property_module()] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &module,
                    ExecutionBudget {
                        fuel: 32,
                        quantum: 32,
                    },
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
        }
    }

    fn assert_dynamic_property_batch<const N: usize>() {
        for module in [
            dynamic_property_module(),
            dynamic_string_property_module(),
            dynamic_numeric_property_module(),
        ] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &module,
                    ExecutionBudget {
                        fuel: 8,
                        quantum: 8,
                    },
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
        }
    }

    fn assert_for_in_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &for_in_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
    }

    fn assert_method_receiver_batch<const N: usize>() {
        let mut isolate = test_isolate();
        let outcome = isolate
            .execute_with_batch::<N>(
                &method_receiver_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::BudgetExhausted);
        let receiver = isolate.fiber.frames[0].base;
        let receiver = isolate.fiber.registers[receiver as usize];
        assert_eq!(isolate.fiber.frames.last().unwrap().this_value, receiver);
    }

    fn assert_catch_batch<const N: usize>() {
        for module in [direct_catch_module(), cross_frame_catch_module()] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &module,
                    ExecutionBudget {
                        fuel: 32,
                        quantum: 32,
                    },
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
        }
    }

    fn assert_construct_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &construct_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_instanceof_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &instanceof_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    #[test]
    fn interpreter_executes_int32_arithmetic() {
        assert_batch_result::<1>();
        assert_batch_result::<2>();
        assert_batch_result::<4>();
        assert_batch_result::<8>();
        assert_batch_result::<16>();
    }

    #[test]
    fn numeric_less_than_works_for_every_dispatch_batch() {
        assert_less_than_batch::<1>();
        assert_less_than_batch::<2>();
        assert_less_than_batch::<4>();
        assert_less_than_batch::<8>();
        assert_less_than_batch::<16>();
    }

    #[test]
    fn typeof_and_string_equality_work_for_every_dispatch_batch() {
        assert_typeof_batch::<1>();
        assert_typeof_batch::<2>();
        assert_typeof_batch::<4>();
        assert_typeof_batch::<8>();
        assert_typeof_batch::<16>();
    }

    #[test]
    fn script_var_declaration_works_for_every_dispatch_batch() {
        assert_scope_batch::<1>();
        assert_scope_batch::<2>();
        assert_scope_batch::<4>();
        assert_scope_batch::<8>();
        assert_scope_batch::<16>();
    }

    #[test]
    fn global_lexical_access_works_for_every_dispatch_batch() {
        assert_global_lexical_batch::<1>();
        assert_global_lexical_batch::<2>();
        assert_global_lexical_batch::<4>();
        assert_global_lexical_batch::<8>();
        assert_global_lexical_batch::<16>();
    }

    #[test]
    fn function_prototype_call_forwards_arguments_for_every_dispatch_batch() {
        assert_function_prototype_call_batch::<1>();
        assert_function_prototype_call_batch::<2>();
        assert_function_prototype_call_batch::<4>();
        assert_function_prototype_call_batch::<8>();
        assert_function_prototype_call_batch::<16>();
    }

    #[test]
    fn native_continuation_resumes_for_every_dispatch_batch_and_forced_major() {
        assert_number_continuation_batch::<1>();
        assert_number_continuation_batch::<2>();
        assert_number_continuation_batch::<4>();
        assert_number_continuation_batch::<8>();
        assert_number_continuation_batch::<16>();
    }

    #[test]
    fn native_continuation_throw_reaches_original_call_site_for_every_dispatch_batch() {
        assert_number_continuation_throw_batch::<1>();
        assert_number_continuation_throw_batch::<2>();
        assert_number_continuation_throw_batch::<4>();
        assert_number_continuation_throw_batch::<8>();
        assert_number_continuation_throw_batch::<16>();
    }

    #[test]
    fn string_hint_continuation_resumes_for_every_dispatch_batch_and_forced_major() {
        assert_string_continuation_batch::<1>();
        assert_string_continuation_batch::<2>();
        assert_string_continuation_batch::<4>();
        assert_string_continuation_batch::<8>();
        assert_string_continuation_batch::<16>();
    }

    #[test]
    fn numeric_unary_continuations_resume_for_every_dispatch_batch_and_forced_major() {
        assert_numeric_unary_continuation_batch::<1>();
        assert_numeric_unary_continuation_batch::<2>();
        assert_numeric_unary_continuation_batch::<4>();
        assert_numeric_unary_continuation_batch::<8>();
        assert_numeric_unary_continuation_batch::<16>();
    }

    #[test]
    fn numeric_binary_continuations_resume_for_every_dispatch_batch_and_forced_major() {
        assert_numeric_binary_continuation_batch::<1>();
        assert_numeric_binary_continuation_batch::<2>();
        assert_numeric_binary_continuation_batch::<4>();
        assert_numeric_binary_continuation_batch::<8>();
        assert_numeric_binary_continuation_batch::<16>();
    }

    #[test]
    fn bound_argument_prefix_forwards_for_every_dispatch_batch() {
        assert_bound_function_batch::<1>();
        assert_bound_function_batch::<2>();
        assert_bound_function_batch::<4>();
        assert_bound_function_batch::<8>();
        assert_bound_function_batch::<16>();
    }

    #[test]
    fn array_push_method_call_is_stable_for_every_dispatch_batch() {
        assert_array_push_batch::<1>();
        assert_array_push_batch::<2>();
        assert_array_push_batch::<4>();
        assert_array_push_batch::<8>();
        assert_array_push_batch::<16>();
    }

    #[test]
    fn strict_and_sloppy_this_binding_work_for_every_dispatch_batch() {
        assert_this_binding_batch::<1>();
        assert_this_binding_batch::<2>();
        assert_this_binding_batch::<4>();
        assert_this_binding_batch::<8>();
        assert_this_binding_batch::<16>();
    }

    #[test]
    fn strict_and_sloppy_unresolved_assignment_work_for_every_dispatch_batch() {
        assert_reference_error_batch::<1>();
        assert_reference_error_batch::<2>();
        assert_reference_error_batch::<4>();
        assert_reference_error_batch::<8>();
        assert_reference_error_batch::<16>();
    }

    #[test]
    fn non_callable_values_throw_type_error_for_every_dispatch_batch() {
        assert_non_callable_batch::<1>();
        assert_non_callable_batch::<2>();
        assert_non_callable_batch::<4>();
        assert_non_callable_batch::<8>();
        assert_non_callable_batch::<16>();
    }

    #[test]
    fn native_error_constructor_hierarchy_survives_forced_major() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &native_error_constructor_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    #[test]
    fn native_function_intrinsics_survive_forced_major_before_forwarding() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &function_prototype_call_module(),
                ExecutionBudget {
                    fuel: 7,
                    quantum: 7,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    fn boxed_number_data_and_properties_survive_forced_major_allocations() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let prototype = isolate.realm.number_prototype.unwrap();
        let boxed = isolate
            .allocate_number_object(Value::from_i32(-7), prototype, AllocationSpace::Young)
            .unwrap();
        isolate.fiber.registers.push(boxed);
        let key = isolate.intern_intrinsic_name(b"field").unwrap();
        isolate
            .set_own_data_property(boxed, key, Value::from_i32(11))
            .unwrap();
        assert_eq!(isolate.this_number_value(boxed).unwrap().as_i32(), Some(-7));
        assert_eq!(
            isolate
                .get_data_property(boxed, key)
                .unwrap()
                .unwrap()
                .as_i32(),
            Some(11)
        );
        assert_eq!(
            isolate.object_snapshot(boxed).unwrap().1.prototype,
            prototype
        );
    }

    #[test]
    /// Keeps a pending Symbol description live before allocation and through a later full major.
    fn symbol_description_survives_forced_major_allocations() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let description = isolate
            .allocate_runtime_string(JsString::try_from_latin1(b"description").unwrap())
            .unwrap();
        let symbol = isolate.allocate_symbol(Some(description)).unwrap();
        isolate.fiber.registers.push(symbol);
        isolate
            .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
            .unwrap();
        let raw = symbol.as_heap_ref().unwrap();
        let symbol = isolate
            .heap
            .checked_reference(raw, isolate.types.symbol)
            .unwrap();
        let description = isolate.heap.with_running_scope(|scope| {
            let symbol = scope.root(symbol).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(symbol, isolate.types.symbol)
                    .map(|symbol| symbol.description.unwrap())
                    .unwrap()
            })
        });
        let mut units = Vec::new();
        isolate
            .append_primitive_string_units(description, &mut units)
            .unwrap();
        assert_eq!(units, "description".encode_utf16().collect::<Vec<_>>());
    }

    #[test]
    fn bound_function_payload_and_name_survive_forced_major_allocations() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &bound_function_call_module(),
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    fn array_identity_and_property_storage_survive_forced_major_allocations() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &array_push_module(),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
    }

    #[test]
    fn array_flat_work_stack_survives_forced_major_allocations() {
        let mut isolate = test_isolate();
        let prototype = isolate.realm.array_prototype.unwrap();
        let inner = isolate
            .create_array_object_with_prototype(prototype)
            .unwrap();
        let outer = isolate
            .create_array_object_with_prototype(prototype)
            .unwrap();
        let zero = isolate.safe_integer_property_atom(0).unwrap();
        let length = isolate.length_atom().unwrap();
        isolate
            .set_own_data_property(inner, zero, Value::from_i32(42))
            .unwrap();
        isolate
            .set_own_data_property(inner, length, Value::from_i32(1))
            .unwrap();
        isolate.set_own_data_property(outer, zero, inner).unwrap();
        isolate
            .set_own_data_property(outer, length, Value::from_i32(1))
            .unwrap();
        isolate.fiber.registers = vec![outer, Value::from_i32(1)];
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let result = isolate
            .array_flat(&CallSite {
                caller_base: 0,
                destination: 0,
                callee: isolate.realm.array_flat.unwrap(),
                argument_base: 1,
                argument_prefix: None,
                argument_prefix_offset: 0,
                argument_prefix_count: 0,
                argument_count: 1,
                this_value: outer,
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: WordOffset::new(0),
            })
            .unwrap();
        assert_eq!(
            isolate
                .get_data_property(result, zero)
                .unwrap()
                .and_then(Value::as_i32),
            Some(42)
        );
    }

    #[test]
    fn nested_bound_construct_preserves_each_new_target_substitution() {
        let mut isolate = test_isolate();
        let target = isolate.realm.array_constructor.unwrap();
        let first = create_test_bound_function(&mut isolate, target);
        let second = create_test_bound_function(&mut isolate, first);

        let (resolved, new_target) = isolate
            .resolve_bound_construct_target(second, first)
            .unwrap();
        assert_eq!((resolved, new_target), (target, target));
        let (resolved, new_target) = isolate
            .resolve_bound_construct_target(second, second)
            .unwrap();
        assert_eq!((resolved, new_target), (target, target));
    }

    /// Exercises raw native helpers before the managed-error dispatch boundary.
    #[test]
    fn array_push_updates_existing_indexed_storage_and_length() {
        let mut isolate = test_isolate();
        let prototype = isolate.realm.array_prototype.unwrap();
        let array = isolate
            .create_array_object_with_prototype(prototype)
            .unwrap();
        let zero = isolate.property_key_atom(Value::from_i32(0)).unwrap();
        isolate
            .set_own_data_property(array, zero, Value::from_i32(1))
            .unwrap();
        let length = isolate.length_atom().unwrap();
        isolate
            .set_own_data_property(array, length, Value::from_i32(1))
            .unwrap();
        isolate.fiber.registers = vec![array, Value::from_i32(2), Value::from_i32(3)];
        let result = isolate
            .array_push(&CallSite {
                caller_base: 0,
                destination: 0,
                callee: isolate.realm.array_push.unwrap(),
                argument_base: 1,
                argument_prefix: None,
                argument_prefix_offset: 0,
                argument_prefix_count: 0,
                argument_count: 2,
                this_value: array,
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: WordOffset::new(0),
            })
            .unwrap();
        assert_eq!(result.as_i32(), Some(3));
        assert_eq!(
            isolate
                .get_data_property(array, length)
                .unwrap()
                .unwrap()
                .as_i32(),
            Some(3)
        );
        let separator = isolate
            .allocate_runtime_string(JsString::try_from_latin1(b"-").unwrap())
            .unwrap();
        isolate.fiber.registers.push(separator);
        let joined = isolate
            .array_join(&CallSite {
                caller_base: 0,
                destination: 0,
                callee: isolate.realm.array_join.unwrap(),
                argument_base: 3,
                argument_prefix: None,
                argument_prefix_offset: 0,
                argument_prefix_count: 0,
                argument_count: 1,
                this_value: array,
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: WordOffset::new(0),
            })
            .unwrap();
        let expected = isolate
            .allocate_runtime_string(JsString::try_from_latin1(b"1-2-3").unwrap())
            .unwrap();
        assert!(isolate.strict_equal_values(joined, expected).unwrap());
    }

    #[test]
    /// Forces collection between loaded literals so pending and published caches must both trace.
    fn loaded_string_constants_survive_forced_major_during_module_load() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &string_constant_root_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::False)
        ));
    }

    #[test]
    fn isolate_owns_the_atom_table_created_from_mandatory_host_config() {
        let mut isolate = test_isolate();
        let mandatory_entries = isolate.atoms().stats().entries;
        let atom = isolate
            .atoms_mut()
            .try_intern(JsString::try_from_str("property").unwrap())
            .unwrap();

        assert_eq!(atom.index() as usize, mandatory_entries);
        assert_eq!(isolate.atoms().get(atom).unwrap().len(), 8);
        assert_eq!(isolate.atoms().stats().entries, mandatory_entries + 1);
    }

    #[test]
    fn primitive_realm_intrinsics_use_stable_non_writable_slots() {
        let mut isolate = test_isolate();
        let infinity = isolate
            .atoms
            .try_intern(JsString::try_from_str("Infinity").unwrap())
            .unwrap();
        let slot = isolate.realm.resolve_intrinsic(infinity).unwrap();

        assert_eq!(
            isolate
                .scope_value(ScopeResolution {
                    atom: infinity,
                    lexical_slot: None,
                    intrinsic_slot: Some(slot),
                    global_slot: None,
                })
                .unwrap()
                .and_then(Value::as_f64),
            Some(f64::INFINITY)
        );
        assert_eq!(
            isolate.realm.set_intrinsic(slot, Value::from_i32(1)),
            Err(ExecutionError::ReadOnlyBinding(infinity))
        );
    }

    #[test]
    fn var_declaration_does_not_publish_over_primitive_realm_intrinsics() {
        let mut isolate = test_isolate();
        let infinity = isolate
            .atoms
            .try_intern(JsString::try_from_str("Infinity").unwrap())
            .unwrap();
        isolate
            .declare_scope_resolution(ScopeResolution {
                atom: infinity,
                lexical_slot: None,
                intrinsic_slot: isolate.realm.resolve_intrinsic(infinity),
                global_slot: None,
            })
            .unwrap();
        assert!(isolate.realm.global_bindings.is_empty());
    }

    #[test]
    /// Proves sparse atom publication resolves through stable slots without binding-order scans.
    fn realm_global_slots_are_stable_and_atom_indexed() {
        let mut isolate = test_isolate();
        let unused = isolate
            .atoms
            .try_intern(JsString::try_from_str("unused-property-name").unwrap())
            .unwrap();
        let first = isolate
            .atoms
            .try_intern(JsString::try_from_str("first-global").unwrap())
            .unwrap();
        let second = isolate
            .atoms
            .try_intern(JsString::try_from_str("second-global").unwrap())
            .unwrap();

        isolate.realm.set(first, Value::from_i32(1)).unwrap();
        let first_slot = isolate.realm.resolve(first).unwrap();
        isolate.realm.set(second, Value::from_i32(2)).unwrap();
        isolate.realm.set(first, Value::from_i32(3)).unwrap();

        assert!(isolate.realm.resolve(unused).is_none());
        assert_eq!(isolate.realm.resolve(first), Some(first_slot));
        assert_eq!(
            isolate.realm.get_slot(first_slot).and_then(Value::as_i32),
            Some(3)
        );
        assert_eq!(
            isolate
                .realm
                .resolve(second)
                .and_then(|slot| isolate.realm.get_slot(slot))
                .and_then(Value::as_i32),
            Some(2)
        );
        assert_eq!(
            isolate.realm.global_bindings[first_slot.index()].name,
            first
        );
        assert_eq!(
            isolate.realm.global_slots_by_atom.len(),
            second.index() as usize + 1
        );
    }

    #[test]
    /// Confirms loaded scope operands self-resolve once and retain the stable realm slot.
    fn loaded_scope_resolution_caches_a_published_global_slot() {
        let mut isolate = test_isolate();
        let module = scoped_var_module();
        let code = isolate.load_module(&module).unwrap();
        assert!(
            isolate.loaded_code[code.index()].scope_resolutions[0]
                .global_slot
                .is_none()
        );

        let outcome = isolate
            .execute_loaded(
                code,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();

        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
        let resolution = isolate.loaded_code[code.index()].scope_resolutions[0];
        let slot = resolution.global_slot.expect("executed binding is cached");
        assert_eq!(isolate.realm.resolve(resolution.atom), Some(slot));
        assert_eq!(
            isolate.realm.get_slot(slot).and_then(Value::as_i32),
            Some(7)
        );
    }

    #[test]
    /// Pins the cursor's unsafe backing invariant across owner moves and Vec reallocations.
    fn bytecode_cursor_survives_owner_and_container_moves() {
        let module = arithmetic_module();
        let words = module
            .function(module.entry_function())
            .unwrap()
            .bytecode()
            .bytecode()
            .words();
        let cursor = BytecodeCursor::new(words);
        let mut owners = Vec::with_capacity(1);
        owners.push(module);
        owners.extend((0..32).map(|_| arithmetic_module()));

        let instruction = cursor.decode(WordOffset::new(0)).unwrap();
        assert_eq!(instruction.opcode, Opcode::LoadImmediate);
        assert_eq!(owners.len(), 33);
    }

    #[test]
    fn interpreter_stops_at_exact_budget_boundary() {
        assert_batch_budget::<1>();
        assert_batch_budget::<2>();
        assert_batch_budget::<4>();
        assert_batch_budget::<8>();
        assert_batch_budget::<16>();
    }

    #[test]
    fn interpreter_rejects_zero_batch_without_panicking() {
        let result = test_isolate().execute_with_batch::<0>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 1,
                quantum: 1,
            },
        );
        assert_eq!(
            result,
            Err(ExecutionError::InvalidDispatchBatch { batch: 0 })
        );
    }

    #[test]
    fn public_execute_uses_the_tuned_non_scalar_dispatch_batch() {
        assert_eq!(tuning::dispatch::DEFAULT_DISPATCH_BATCH, 8);
        let outcome = test_isolate()
            .execute(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    #[test]
    fn interpreter_restarts_dispatch_after_conditional_jumps() {
        assert_conditional_batch::<1>();
        assert_conditional_batch::<2>();
        assert_conditional_batch::<4>();
        assert_conditional_batch::<8>();
        assert_conditional_batch::<16>();
    }

    #[test]
    fn interpreter_restarts_dispatch_after_backward_jumps() {
        assert_backedge_batch::<1>();
        assert_backedge_batch::<2>();
        assert_backedge_batch::<4>();
        assert_backedge_batch::<8>();
        assert_backedge_batch::<16>();
    }

    #[test]
    fn for_in_iterator_loop_is_stable_for_every_dispatch_batch() {
        assert_for_in_batch::<1>();
        assert_for_in_batch::<2>();
        assert_for_in_batch::<4>();
        assert_for_in_batch::<8>();
        assert_for_in_batch::<16>();
    }

    #[test]
    fn for_in_iterator_and_returned_keys_survive_forced_major() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &for_in_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
    }

    #[test]
    fn logical_short_circuit_preserves_operands_for_every_dispatch_batch() {
        assert_logical_batch::<1>();
        assert_logical_batch::<2>();
        assert_logical_batch::<4>();
        assert_logical_batch::<8>();
        assert_logical_batch::<16>();
    }

    #[test]
    fn switch_dispatch_chain_is_stable_for_every_dispatch_batch() {
        assert_switch_batch::<1>();
        assert_switch_batch::<2>();
        assert_switch_batch::<4>();
        assert_switch_batch::<8>();
        assert_switch_batch::<16>();
    }

    #[test]
    fn explicit_function_frames_work_for_every_dispatch_batch() {
        assert_call_batch::<1>();
        assert_call_batch::<2>();
        assert_call_batch::<4>();
        assert_call_batch::<8>();
        assert_call_batch::<16>();
    }

    #[test]
    fn zero_register_undefined_returns_work_for_every_dispatch_batch() {
        assert_undefined_call_batch::<1>();
        assert_undefined_call_batch::<2>();
        assert_undefined_call_batch::<4>();
        assert_undefined_call_batch::<8>();
        assert_undefined_call_batch::<16>();
    }

    #[test]
    fn captured_environments_work_for_every_dispatch_batch() {
        assert_captured_environment_batch::<1>();
        assert_captured_environment_batch::<2>();
        assert_captured_environment_batch::<4>();
        assert_captured_environment_batch::<8>();
        assert_captured_environment_batch::<16>();
    }

    #[test]
    fn captured_environment_survives_forced_major_allocation() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<8>(
                &captured_environment_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    #[test]
    fn callee_throw_exits_every_dispatch_batch_without_native_unwind() {
        assert_throw_batch::<1>();
        assert_throw_batch::<2>();
        assert_throw_batch::<4>();
        assert_throw_batch::<8>();
        assert_throw_batch::<16>();
    }

    #[test]
    fn cross_module_call_switches_code_for_every_dispatch_batch() {
        assert_cross_code_batch::<1>();
        assert_cross_code_batch::<2>();
        assert_cross_code_batch::<4>();
        assert_cross_code_batch::<8>();
        assert_cross_code_batch::<16>();
    }

    #[test]
    fn ordinary_property_replacement_and_update_work_for_every_dispatch_batch() {
        assert_property_batch::<1>();
        assert_property_batch::<2>();
        assert_property_batch::<4>();
        assert_property_batch::<8>();
        assert_property_batch::<16>();
    }

    #[test]
    fn computed_property_access_works_for_every_dispatch_batch() {
        assert_dynamic_property_batch::<1>();
        assert_dynamic_property_batch::<2>();
        assert_dynamic_property_batch::<4>();
        assert_dynamic_property_batch::<8>();
        assert_dynamic_property_batch::<16>();
    }

    #[test]
    /// Forces collection during first numeric-key publication to exercise the shared rooting path.
    fn computed_property_publication_roots_receiver_across_forced_major() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &dynamic_property_module(),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    fn integer_property_keys_preserve_ecmascript_decimal_spelling() {
        for (value, expected) in [
            (i32::MIN, "-2147483648"),
            (-1, "-1"),
            (0, "0"),
            (i32::MAX, "2147483647"),
        ] {
            let key = Int32PropertyKey::new(value);
            assert_eq!(key.as_bytes(), expected.as_bytes());
        }
    }

    #[test]
    fn method_calls_preserve_receiver_for_every_dispatch_batch() {
        assert_method_receiver_batch::<1>();
        assert_method_receiver_batch::<2>();
        assert_method_receiver_batch::<4>();
        assert_method_receiver_batch::<8>();
        assert_method_receiver_batch::<16>();
    }

    #[test]
    fn catch_dispatch_and_cross_frame_throw_work_for_every_dispatch_batch() {
        assert_catch_batch::<1>();
        assert_catch_batch::<2>();
        assert_catch_batch::<4>();
        assert_catch_batch::<8>();
        assert_catch_batch::<16>();
    }

    #[test]
    fn construct_receiver_and_primitive_return_work_for_every_dispatch_batch() {
        assert_construct_batch::<1>();
        assert_construct_batch::<2>();
        assert_construct_batch::<4>();
        assert_construct_batch::<8>();
        assert_construct_batch::<16>();
    }

    #[test]
    fn instanceof_walks_prototypes_for_every_dispatch_batch() {
        assert_instanceof_batch::<1>();
        assert_instanceof_batch::<2>();
        assert_instanceof_batch::<4>();
        assert_instanceof_batch::<8>();
        assert_instanceof_batch::<16>();
    }

    #[test]
    /// Forced major collections cover closure prototype creation and receiver chain publication.
    fn instanceof_prototype_chain_survives_forced_major() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &instanceof_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    #[test]
    /// Plain closure creation stays allocation-light until prototype observation materializes it.
    fn function_prototype_is_lazily_materialized_with_constructor_back_reference() {
        let mut isolate = test_isolate();
        let outcome = isolate
            .execute_with_batch::<8>(
                &call_module(),
                ExecutionBudget {
                    fuel: 1,
                    quantum: 1,
                },
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::BudgetExhausted);
        let function = isolate.fiber.registers[0];
        let reference = isolate
            .heap
            .checked_reference(function.as_heap_ref().unwrap(), isolate.types.function)
            .unwrap();
        let before = isolate.heap.with_running_scope(|scope| {
            let function = scope.root(reference).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(function, isolate.types.function)
                    .unwrap()
                    .function_prototype
            })
        });
        assert!(before.is_none());

        let prototype_atom = isolate.prototype_atom().unwrap();
        let prototype = isolate
            .get_data_property(function, prototype_atom)
            .unwrap()
            .unwrap();
        let constructor_atom = isolate.constructor_atom().unwrap();
        assert_eq!(
            isolate
                .get_data_property(prototype, constructor_atom)
                .unwrap(),
            Some(function)
        );
    }

    #[test]
    fn construct_receiver_stays_rooted_across_forced_major_property_allocation() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &construct_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    fn property_publication_roots_receiver_and_heap_value_across_forced_major() {
        for module in [
            heap_value_property_module(),
            function_heap_value_property_module(),
        ] {
            let mut isolate = test_isolate();
            isolate
                .heap
                .set_forced_collection_mode(ForcedCollectionMode::Major);
            let outcome = isolate
                .execute(
                    &module,
                    ExecutionBudget {
                        fuel: 16,
                        quantum: 16,
                    },
                )
                .unwrap();
            let RunOutcome::Completed(child) = outcome else {
                panic!("property fixture must complete");
            };
            assert!(isolate.object_snapshot(child).is_ok());
        }
    }

    #[test]
    /// Confirms verifier-produced stack depths become allocation reservations before dispatch.
    fn entry_reserves_verified_execution_windows() {
        let layout = FunctionLayout {
            register_count: 2,
            max_handler_depth: 3,
            max_completion_depth: 4,
            ..FunctionLayout::default()
        };
        let mut isolate = test_isolate();
        let module = state_module(FunctionKind::Module, layout);
        let code = isolate.load_module(&module).unwrap();
        isolate.enter(code, FunctionId::new(0)).unwrap();

        assert!(isolate.fiber.frames.capacity() >= 1);
        assert!(isolate.fiber.registers.capacity() >= 2);
        assert!(isolate.fiber.handlers.capacity() >= 3);
        assert!(isolate.fiber.completions.capacity() >= 4);
        assert_eq!(
            isolate.fiber.frames[0].strictness,
            FunctionStrictness::Strict
        );
    }

    #[test]
    /// Exercises every fiber-owned GC edge with a tracer that simulates object relocation.
    fn fiber_trace_roots_visits_execution_state_without_native_stack_scanning() {
        let layout = FunctionLayout {
            register_count: 1,
            max_handler_depth: 1,
            max_completion_depth: 2,
            ..FunctionLayout::default()
        };
        let mut isolate = test_isolate();
        let module = state_module(FunctionKind::Script, layout);
        let code = isolate.load_module(&module).unwrap();
        isolate.enter(code, FunctionId::new(0)).unwrap();
        let raw = RawHeapRef::new(16).expect("valid logical address");
        isolate.fiber.registers[0] = Value::from_heap_ref(raw);
        let global = isolate
            .atoms
            .try_intern(JsString::try_from_str("global").unwrap())
            .unwrap();
        isolate
            .realm
            .set(global, Value::from_heap_ref(raw))
            .unwrap();
        let frame = isolate.fiber.frames.last_mut().expect("entry frame exists");
        // SAFETY: this test never dereferences the synthetic environment reference; it only checks
        // that the exact tracing contract rewrites every encoded fiber edge.
        frame.environment = Some(unsafe { GcRef::from_raw_unchecked(raw) });
        frame.return_register = Some(RegisterId::new(0));
        frame.this_value = Value::from_heap_ref(raw);
        frame.new_target = Value::from_heap_ref(raw);
        isolate.fiber.handlers.push(ActiveHandler {
            handler_index: 0,
            frame_depth: 1,
            environment_depth: 1,
        });
        isolate.fiber.completions.extend([
            Completion::Return(Value::from_heap_ref(raw)),
            Completion::Throw(Value::from_heap_ref(raw)),
        ]);
        isolate.fiber.pending_exception = Some(Value::from_heap_ref(raw));
        let mut tracer = RewritingTracer;

        isolate.trace_roots(&mut tracer);

        let rewritten = RawHeapRef::new(32).expect("valid logical address");
        assert_eq!(isolate.fiber.registers[0].as_heap_ref(), Some(rewritten));
        let frame = isolate.fiber.frames.last().expect("entry frame exists");
        assert_eq!(frame.environment.map(GcRef::raw), Some(rewritten));
        assert_eq!(frame.this_value.as_heap_ref(), Some(rewritten));
        assert_eq!(frame.new_target.as_heap_ref(), Some(rewritten));
        assert!(matches!(
            isolate.fiber.completions[0],
            Completion::Return(value) if value.as_heap_ref() == Some(rewritten)
        ));
        assert_eq!(
            isolate.fiber.pending_exception.and_then(Value::as_heap_ref),
            Some(rewritten)
        );
        assert!(matches!(
            isolate.fiber.completions[1],
            Completion::Throw(value) if value.as_heap_ref() == Some(rewritten)
        ));
        assert_eq!(
            isolate
                .realm
                .resolve(global)
                .and_then(|slot| isolate.realm.get_slot(slot))
                .and_then(Value::as_heap_ref),
            Some(rewritten)
        );
    }

    struct RewritingTracer;

    impl Tracer for RewritingTracer {
        fn trace_value(&mut self, value: &mut Value) {
            if let Some(reference) = value.as_heap_ref() {
                *value = Value::from_heap_ref(rewrite(reference));
            }
        }

        fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
            *reference = rewrite(*reference);
        }

        fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
            *reference = reference.map(rewrite);
        }

        fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, value: &mut Value) {
            *key = key.map(rewrite);
            self.trace_value(value);
        }

        fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, held_value: &mut Value) {
            *target = target.map(rewrite);
            self.trace_value(held_value);
        }
    }

    fn rewrite(reference: RawHeapRef) -> RawHeapRef {
        RawHeapRef::new(reference.offset() + 16).expect("test offset stays non-zero")
    }

    fn assert_batch_result<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    fn assert_less_than_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &less_than_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    fn assert_typeof_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &typeof_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    fn assert_scope_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &scoped_var_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
    }

    fn assert_global_lexical_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &global_lexical_module(),
                ExecutionBudget {
                    fuel: 5,
                    quantum: 5,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_function_prototype_call_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &function_prototype_call_module(),
                ExecutionBudget {
                    fuel: 7,
                    quantum: 7,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    /// Exercises callback-local catch, continuation resume, and GC tracing for one dispatch batch.
    fn assert_number_continuation_batch<const N: usize>() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &number_continuation_module(),
                ExecutionBudget {
                    fuel: 24,
                    quantum: 24,
                },
            )
            .unwrap();
        assert!(
            matches!(
                outcome,
                RunOutcome::Completed(value)
                    if value.as_immediate() == Some(Immediate::True)
            ),
            "unexpected continuation outcome: {outcome:?}"
        );
    }

    /// Exercises a callback throw reaching the handler around the original native call site.
    fn assert_number_continuation_throw_batch<const N: usize>() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &number_continuation_throw_module(),
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    /// Exercises String's string-hint callback ordering and continuation tracing for one batch.
    fn assert_string_continuation_batch<const N: usize>() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &string_continuation_module(),
                ExecutionBudget {
                    fuel: 20,
                    quantum: 20,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    /// Exercises numeric unary callback resume and tracing for one dispatch batch.
    fn assert_numeric_unary_continuation_batch<const N: usize>() {
        for (opcode, expected) in [
            (Opcode::ToNumber, 7),
            (Opcode::Negate, -7),
            (Opcode::BitwiseNot, -8),
        ] {
            let mut isolate = test_isolate();
            isolate
                .heap
                .set_forced_collection_mode(ForcedCollectionMode::Major);
            let outcome = isolate
                .execute_with_batch::<N>(
                    &numeric_unary_continuation_module(opcode, expected),
                    ExecutionBudget {
                        fuel: 20,
                        quantum: 20,
                    },
                )
                .unwrap();
            assert!(matches!(
                outcome,
                RunOutcome::Completed(value)
                    if value.as_immediate() == Some(Immediate::True)
            ));
        }
    }

    /// Exercises left and right callback resume while the pending operand remains traced.
    fn assert_numeric_binary_continuation_batch<const N: usize>() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &numeric_binary_continuation_module(),
                ExecutionBudget {
                    fuel: 40,
                    quantum: 40,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    fn assert_bound_function_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &bound_function_call_module(),
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_array_push_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &array_push_module(),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
    }

    /// Creates a zero-argument bound exotic through the production allocation path.
    fn create_test_bound_function(isolate: &mut Isolate, target: Value) -> Value {
        let undefined = Value::from_immediate(Immediate::Undefined);
        isolate.fiber.registers.resize(3, undefined);
        isolate.fiber.registers[1] = target;
        let bound = isolate
            .create_bound_function(&CallSite {
                caller_base: 0,
                destination: 0,
                callee: isolate.realm.function_prototype_bind.unwrap(),
                argument_base: 2,
                argument_prefix: None,
                argument_prefix_offset: 0,
                argument_prefix_count: 0,
                argument_count: 0,
                this_value: target,
                new_target: undefined,
                construct_receiver: None,
                call_site: WordOffset::new(0),
            })
            .unwrap();
        isolate.fiber.registers[1] = bound;
        bound
    }

    /// Checks both nullish substitution and strict preservation in one dispatch monomorphization.
    fn assert_this_binding_batch<const N: usize>() {
        let mut sloppy = test_isolate();
        let global_object = sloppy.realm.global_object.unwrap();
        let outcome = sloppy
            .execute_with_batch::<N>(
                &this_binding_module(FunctionStrictness::Sloppy),
                ExecutionBudget {
                    fuel: 7,
                    quantum: 7,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value == global_object));

        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &this_binding_module(FunctionStrictness::Strict),
                ExecutionBudget {
                    fuel: 7,
                    quantum: 7,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::Undefined)
        ));
    }

    /// Confirms dispatch-level error conversion preserves constructor identity for each batch.
    fn assert_reference_error_batch<const N: usize>() {
        let mut isolate = test_isolate();
        let expected = isolate
            .realm
            .error_intrinsics
            .get(NativeErrorKind::Reference)
            .constructor
            .unwrap();
        let outcome = isolate
            .execute_with_batch::<N>(
                &unresolved_assignment_module(FunctionStrictness::Strict),
                ExecutionBudget {
                    fuel: 2,
                    quantum: 2,
                },
            )
            .unwrap();
        let RunOutcome::Thrown(error) = outcome else {
            panic!("strict unresolved assignment must throw");
        };
        assert_eq!(
            isolate.native_error_kind(error).unwrap(),
            Some(NativeErrorKind::Reference)
        );
        let constructor_atom = isolate.constructor_atom().unwrap();
        let constructor = isolate.get_data_property(error, constructor_atom).unwrap();
        assert_eq!(constructor, Some(expected));

        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &unresolved_assignment_module(FunctionStrictness::Sloppy),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    /// Confirms the raw callable fast path retains language-level error conversion.
    fn assert_non_callable_batch<const N: usize>() {
        let mut isolate = test_isolate();
        let outcome = isolate
            .execute_with_batch::<N>(
                &non_callable_module(),
                ExecutionBudget {
                    fuel: 2,
                    quantum: 2,
                },
            )
            .unwrap();
        let RunOutcome::Thrown(error) = outcome else {
            panic!("integer call must throw");
        };
        assert_eq!(
            isolate.native_error_kind(error).unwrap(),
            Some(NativeErrorKind::Type)
        );
    }

    /// Confirms a zero-register callee returns undefined through the caller destination.
    fn assert_undefined_call_batch<const N: usize>() {
        let module = undefined_call_module();
        assert_eq!(
            module
                .function(FunctionId::new(1))
                .unwrap()
                .layout()
                .register_count,
            0
        );
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::Undefined)
        ));
    }

    fn assert_batch_budget<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 3,
                    quantum: 3,
                },
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::BudgetExhausted);
    }

    fn assert_conditional_batch<const N: usize>() {
        for (test, expected) in [(Opcode::LoadTrue, 1), (Opcode::LoadFalse, 2)] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &conditional_module(test),
                    ExecutionBudget {
                        fuel: 6,
                        quantum: 6,
                    },
                )
                .unwrap();
            assert!(
                matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
            );
        }
    }

    fn assert_backedge_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &backedge_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    /// Checks both paths of all logical branches under one dispatch batch monomorphization.
    fn assert_logical_batch<const N: usize>() {
        let cases = [
            (Opcode::JumpIfFalse, Opcode::LoadFalse, None, None),
            (Opcode::JumpIfFalse, Opcode::LoadTrue, None, Some(42)),
            (Opcode::JumpIfTrue, Opcode::LoadFalse, None, Some(42)),
            (Opcode::JumpIfTrue, Opcode::LoadTrue, None, None),
            (Opcode::JumpIfNotNullish, Opcode::LoadNull, None, Some(42)),
            (
                Opcode::JumpIfNotNullish,
                Opcode::LoadImmediate,
                Some(7),
                Some(7),
            ),
        ];
        for (branch, left, immediate, expected_integer) in cases {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &logical_module(branch, left, immediate),
                    ExecutionBudget {
                        fuel: 8,
                        quantum: 8,
                    },
                )
                .unwrap();
            let RunOutcome::Completed(value) = outcome else {
                panic!("logical module must complete");
            };
            if let Some(expected_integer) = expected_integer {
                assert_eq!(value.as_i32(), Some(expected_integer));
            } else {
                let expected = if left == Opcode::LoadFalse {
                    Immediate::False
                } else {
                    Immediate::True
                };
                assert_eq!(value.as_immediate(), Some(expected));
            }
        }
    }

    fn assert_switch_batch<const N: usize>() {
        for (discriminant, expected) in [(1, 10), (2, 20), (9, 20)] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &switch_module(discriminant),
                    ExecutionBudget {
                        fuel: 16,
                        quantum: 16,
                    },
                )
                .unwrap();
            assert!(
                matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
            );
        }
    }

    /// Builds a verified branch program with explicit labels to exercise PC changes inside one dispatch batch.
    fn conditional_module(test: Opcode) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(8, 2);
        let alternate = builder.new_label().unwrap();
        let end = builder.new_label().unwrap();
        builder.emit(test, &[0], span).unwrap();
        builder
            .emit_jump_if_false(RegisterId::new(0), alternate, span)
            .unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        builder.bind_label(alternate).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 2], span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("conditional"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a verified counting loop so every dispatch batch exercises a taken backward jump.
    fn backedge_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(9, 2);
        let condition = builder.new_label().unwrap();
        let end = builder.new_label().unwrap();
        builder.emit(Opcode::LoadImmediate, &[0, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 3], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 1], span).unwrap();
        builder.bind_label(condition).unwrap();
        builder.emit(Opcode::LessThan, &[3, 0, 1], span).unwrap();
        builder
            .emit_jump_if_false(RegisterId::new(3), end, span)
            .unwrap();
        builder.emit(Opcode::Add, &[0, 0, 2], span).unwrap();
        builder.emit_jump(condition, span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[0], span).unwrap();
        single_function_module("backedge", Vec::new(), builder)
    }

    /// Builds two numeric own properties and counts the complete managed iterator snapshot.
    fn for_in_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(24, 2);
        let condition = builder.new_label().unwrap();
        let end = builder.new_label().unwrap();
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 10], span).unwrap();
        builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[3, 1], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[4, 20], span).unwrap();
        builder.emit(Opcode::SetByValue, &[0, 4, 3], span).unwrap();
        builder
            .emit(Opcode::CreateForInIterator, &[5, 0], span)
            .unwrap();
        builder.emit(Opcode::LoadUndefined, &[6], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[7, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[8, 1], span).unwrap();
        builder.bind_label(condition).unwrap();
        builder.emit(Opcode::ForInNext, &[9, 5], span).unwrap();
        builder
            .emit(Opcode::StrictEqual, &[10, 9, 6], span)
            .unwrap();
        builder
            .emit_jump_if_true(RegisterId::new(10), end, span)
            .unwrap();
        builder.emit(Opcode::Add, &[7, 7, 8], span).unwrap();
        builder.emit_jump(condition, span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[7], span).unwrap();
        single_function_module("for-in", Vec::new(), builder)
    }

    /// Builds one operand-preserving short-circuit branch around a right-hand integer load.
    fn logical_module(branch: Opcode, left: Opcode, immediate: Option<u32>) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 1);
        let end = builder.new_label().unwrap();
        if let Some(value) = immediate {
            builder.emit(left, &[0, value], span).unwrap();
        } else {
            builder.emit(left, &[0], span).unwrap();
        }
        builder.emit(Opcode::Move, &[1, 0], span).unwrap();
        match branch {
            Opcode::JumpIfFalse => builder.emit_jump_if_false(RegisterId::new(0), end, span),
            Opcode::JumpIfTrue => builder.emit_jump_if_true(RegisterId::new(0), end, span),
            Opcode::JumpIfNotNullish => {
                builder.emit_jump_if_not_nullish(RegisterId::new(0), end, span)
            }
            _ => panic!("test supplies a logical branch opcode"),
        }
        .unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
        builder.emit(Opcode::Move, &[1, 2], span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("logical"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a two-case dispatch whose middle default deliberately falls through into case two.
    fn switch_module(discriminant: u32) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(16, 4);
        let case_one = builder.new_label().unwrap();
        let default = builder.new_label().unwrap();
        let case_two = builder.new_label().unwrap();
        let end = builder.new_label().unwrap();
        builder
            .emit(Opcode::LoadImmediate, &[0, discriminant], span)
            .unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[2, 0, 1], span).unwrap();
        builder
            .emit_jump_if_true(RegisterId::new(2), case_one, span)
            .unwrap();
        builder.emit(Opcode::LoadImmediate, &[3, 2], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[4, 0, 3], span).unwrap();
        builder
            .emit_jump_if_true(RegisterId::new(4), case_two, span)
            .unwrap();
        builder.emit_jump(default, span).unwrap();
        builder.bind_label(case_one).unwrap();
        builder.emit(Opcode::LoadImmediate, &[5, 10], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        builder.bind_label(default).unwrap();
        builder.emit(Opcode::LoadImmediate, &[5, 30], span).unwrap();
        builder.bind_label(case_two).unwrap();
        builder.emit(Opcode::LoadImmediate, &[5, 20], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[5], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("switch"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds two property additions followed by an allocation-free update and own read.
    fn property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(10, 0);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 41], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 7], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 2, 1], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[3, 42], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 3, 0], span).unwrap();
        builder.emit(Opcode::GetById, &[4, 0, 0], span).unwrap();
        builder.emit(Opcode::Return, &[4], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("properties"),
            Vec::new(),
            vec![Arc::from("answer"), Arc::from("other")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a numeric-key write/read pair over one ordinary object.
    fn dynamic_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 0);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
        builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
        builder.emit(Opcode::GetByValue, &[3, 0, 1], span).unwrap();
        builder.emit(Opcode::Return, &[3], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("dynamic properties"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a string-key write/read pair to cover the non-integer PropertyKey path.
    fn dynamic_string_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(7, 1);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::LoadConstant, &[1, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
        builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
        builder.emit(Opcode::GetByValue, &[3, 0, 1], span).unwrap();
        builder.emit(Opcode::Return, &[3], span).unwrap();
        single_function_module(
            "dynamic string property",
            vec![BytecodeConstant::string_from_utf16(
                "answer".encode_utf16().collect(),
            )],
            builder,
        )
    }

    /// Builds a numeric-key write/string-key read pair to verify ECMAScript formatting.
    fn dynamic_numeric_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(7, 2);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::LoadConstant, &[1, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
        builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
        builder.emit(Opcode::LoadConstant, &[3, 1], span).unwrap();
        builder.emit(Opcode::GetByValue, &[4, 0, 3], span).unwrap();
        builder.emit(Opcode::Return, &[4], span).unwrap();
        single_function_module(
            "dynamic numeric property",
            vec![
                BytecodeConstant::NumberBits(1.2f64.to_bits()),
                BytecodeConstant::string_from_utf16("1.2".encode_utf16().collect()),
            ],
            builder,
        )
    }

    /// Builds a callable carrying the same shape/storage path as an ordinary object.
    fn function_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(6, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("function property"),
            Vec::new(),
            vec![Arc::from("answer")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds `identity.call(undefined, 42)` through the shared native Function prototype.
    fn function_prototype_call_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(6, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::LoadUndefined, &[2], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[3, 42], span).unwrap();
        entry
            .emit(Opcode::CallWithReceiver, &[4, 0, 2], span)
            .unwrap();
        entry.emit(Opcode::Return, &[4], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(1, 0);
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                argument_count: 1,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("identity.call(undefined, 42)"),
            Vec::new(),
            vec![Arc::from("call")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds `(1.25).toFixed({ valueOf() { return 1; } }) === "1.3"` for trampoline tests.
    fn number_continuation_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(13, 2);
        entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[2, 1, 2], span).unwrap();
        entry.emit(Opcode::CreateObject, &[3], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[4, 1], span).unwrap();
        entry.emit(Opcode::SetById, &[3, 4, 3], span).unwrap();
        entry.emit(Opcode::LoadConstant, &[5, 0], span).unwrap();
        entry.emit(Opcode::Move, &[6, 2], span).unwrap();
        entry.emit(Opcode::Move, &[7, 3], span).unwrap();
        entry
            .emit(Opcode::CallWithReceiver, &[8, 5, 1], span)
            .unwrap();
        entry.emit(Opcode::LoadConstant, &[9, 1], span).unwrap();
        entry.emit(Opcode::StrictEqual, &[10, 8, 9], span).unwrap();
        entry.emit(Opcode::Return, &[10], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callback = BytecodeBuilder::with_capacity(6, 1);
        let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
        callback
            .emit(Opcode::LoadImmediate, &[0, 42], span)
            .unwrap();
        callback.emit(Opcode::Throw, &[0], span).unwrap();
        let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
        callback.emit(Opcode::LoadImmediate, &[2, 1], span).unwrap();
        callback.emit(Opcode::Return, &[2], span).unwrap();
        let (callback_bytecode, callback_source_map, callback_registers) =
            callback.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let mut callback_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callback_registers,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map: callback_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        callback_metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        CompiledModule::new(
            Arc::from("Number ToPrimitive continuation"),
            vec![
                BytecodeConstant::NumberBits(1.25_f64.to_bits()),
                BytecodeConstant::string_from_utf16("1.3".encode_utf16().collect()),
            ],
            vec![
                Arc::from("Number"),
                Arc::from("prototype"),
                Arc::from("toFixed"),
                Arc::from("valueOf"),
            ],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callback_bytecode,
                    callback_metadata,
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a protected Number call whose `valueOf` callback throws through its continuation.
    fn number_continuation_throw_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(16, 2);
        let end = entry.new_label().unwrap();
        entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[2, 1, 2], span).unwrap();
        entry.emit(Opcode::CreateObject, &[3], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[4, 1], span).unwrap();
        entry.emit(Opcode::SetById, &[3, 4, 3], span).unwrap();
        entry.emit(Opcode::LoadConstant, &[5, 0], span).unwrap();
        entry.emit(Opcode::Move, &[6, 2], span).unwrap();
        entry.emit(Opcode::Move, &[7, 3], span).unwrap();
        let protected_start = entry.emit(Opcode::Nop, &[], span).unwrap();
        entry
            .emit(Opcode::CallWithReceiver, &[8, 5, 1], span)
            .unwrap();
        entry.emit_jump(end, span).unwrap();
        let handler = entry.emit(Opcode::LoadException, &[9], span).unwrap();
        entry.emit(Opcode::Return, &[9], span).unwrap();
        entry.bind_label(end).unwrap();
        entry.emit(Opcode::LoadUndefined, &[10], span).unwrap();
        entry.emit(Opcode::Return, &[10], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callback = BytecodeBuilder::with_capacity(2, 0);
        callback
            .emit(Opcode::LoadImmediate, &[0, 42], span)
            .unwrap();
        callback.emit(Opcode::Throw, &[0], span).unwrap();
        let (callback_bytecode, callback_source_map, callback_registers) =
            callback.finish().unwrap();
        let mut entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        entry_metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        let callback_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callback_registers,
                ..FunctionLayout::default()
            },
            source_map: callback_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("Number continuation callback throw"),
            vec![BytecodeConstant::NumberBits(1.25_f64.to_bits())],
            vec![
                Arc::from("Number"),
                Arc::from("prototype"),
                Arc::from("toFixed"),
                Arc::from("valueOf"),
            ],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callback_bytecode,
                    callback_metadata,
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds `String({ toString() { try/catch; return "converted"; } })` for trampoline tests.
    fn string_continuation_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(8, 2);
        entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
        entry.emit(Opcode::CreateObject, &[1], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[2, 1], span).unwrap();
        entry.emit(Opcode::SetById, &[1, 2, 1], span).unwrap();
        entry.emit(Opcode::Call, &[3, 0, 1], span).unwrap();
        entry.emit(Opcode::LoadConstant, &[4, 0], span).unwrap();
        entry.emit(Opcode::StrictEqual, &[5, 3, 4], span).unwrap();
        entry.emit(Opcode::Return, &[5], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callback = BytecodeBuilder::with_capacity(6, 1);
        let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
        callback.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
        callback.emit(Opcode::Throw, &[0], span).unwrap();
        let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
        callback.emit(Opcode::LoadConstant, &[2, 0], span).unwrap();
        callback.emit(Opcode::Return, &[2], span).unwrap();
        let (callback_bytecode, callback_source_map, callback_registers) =
            callback.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let mut callback_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callback_registers,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map: callback_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        callback_metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        CompiledModule::new(
            Arc::from("String ToPrimitive continuation"),
            vec![BytecodeConstant::string_from_utf16(
                "converted".encode_utf16().collect(),
            )],
            vec![Arc::from("String"), Arc::from("toString")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callback_bytecode,
                    callback_metadata,
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a callback-driven numeric unary expression and compares its exact result.
    fn numeric_unary_continuation_module(opcode: Opcode, expected: i32) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(7, 1);
        entry.emit(Opcode::CreateObject, &[0], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(opcode, &[2, 0], span).unwrap();
        entry
            .emit(Opcode::LoadImmediate, &[3, expected as u32], span)
            .unwrap();
        entry.emit(Opcode::StrictEqual, &[4, 2, 3], span).unwrap();
        entry.emit(Opcode::Return, &[4], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callback = BytecodeBuilder::with_capacity(6, 1);
        let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
        callback.emit(Opcode::LoadImmediate, &[0, 8], span).unwrap();
        callback.emit(Opcode::Throw, &[0], span).unwrap();
        let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
        callback.emit(Opcode::LoadImmediate, &[2, 7], span).unwrap();
        callback.emit(Opcode::Return, &[2], span).unwrap();
        let (callback_bytecode, callback_source_map, callback_registers) =
            callback.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let mut callback_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callback_registers,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map: callback_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        callback_metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        CompiledModule::new(
            Arc::from("numeric unary continuation"),
            Vec::new(),
            vec![Arc::from("valueOf")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callback_bytecode,
                    callback_metadata,
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds two callback-driven operands for an exact subtraction continuation result.
    fn numeric_binary_continuation_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(10, 1);
        entry.emit(Opcode::CreateObject, &[0], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(Opcode::CreateObject, &[2], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[3, 2], span).unwrap();
        entry.emit(Opcode::SetById, &[2, 3, 0], span).unwrap();
        entry.emit(Opcode::Sub, &[4, 0, 2], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[5, 6], span).unwrap();
        entry.emit(Opcode::StrictEqual, &[6, 4, 5], span).unwrap();
        entry.emit(Opcode::Return, &[6], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("numeric binary continuation"),
            Vec::new(),
            vec![Arc::from("valueOf")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                numeric_callback_template(FunctionId::new(1), 8, span),
                numeric_callback_template(FunctionId::new(2), 2, span),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a callback with an internal throw/catch before returning one numeric primitive.
    fn numeric_callback_template(
        function: FunctionId,
        returned: i32,
        span: SourceSpan,
    ) -> CompiledFunctionTemplate {
        let mut callback = BytecodeBuilder::with_capacity(6, 1);
        let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
        callback.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
        callback.emit(Opcode::Throw, &[0], span).unwrap();
        let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
        callback
            .emit(Opcode::LoadImmediate, &[2, returned as u32], span)
            .unwrap();
        callback.emit(Opcode::Return, &[2], span).unwrap();
        let (bytecode, source_map, register_count) = callback.finish().unwrap();
        let mut metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        CompiledFunctionTemplate::new(function, bytecode, metadata)
    }

    /// Builds `add.bind(undefined, 20)(22)` with one immutable bound-argument prefix.
    fn bound_function_call_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(8, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::LoadUndefined, &[2], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[3, 20], span).unwrap();
        entry
            .emit(Opcode::CallWithReceiver, &[4, 0, 2], span)
            .unwrap();
        entry.emit(Opcode::LoadImmediate, &[5, 22], span).unwrap();
        entry.emit(Opcode::Call, &[6, 4, 1], span).unwrap();
        entry.emit(Opcode::Return, &[6], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
        callee.emit(Opcode::Return, &[2], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        CompiledModule::new(
            Arc::from("add.bind(undefined, 20)(22)"),
            Vec::new(),
            vec![Arc::from("bind"), Arc::from("add")],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callee_bytecode,
                    FunctionMetadata {
                        layout: FunctionLayout {
                            register_count: callee_registers,
                            argument_count: 2,
                            function_length: 2,
                            name_scope: Some(1),
                            ..FunctionLayout::default()
                        },
                        source_map: callee_source_map,
                        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds `[].push(20, 22)` through a verified receiver/callee/argument window.
    fn array_push_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 0);
        builder.emit(Opcode::CreateArray, &[0], span).unwrap();
        builder.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 20], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[3, 22], span).unwrap();
        builder
            .emit(Opcode::CallWithReceiver, &[4, 0, 2], span)
            .unwrap();
        builder.emit(Opcode::Return, &[4], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        CompiledModule::new(
            Arc::from("[].push(20, 22)"),
            Vec::new(),
            vec![Arc::from("push")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count,
                        ..FunctionLayout::default()
                    },
                    source_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds `readThis.call(undefined)` with caller-selected immutable function strictness.
    fn this_binding_module(strictness: FunctionStrictness) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(5, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::LoadUndefined, &[2], span).unwrap();
        entry
            .emit(Opcode::CallWithReceiver, &[3, 0, 1], span)
            .unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadThis, &[0], span).unwrap();
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            strictness,
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("readThis.call(undefined)"),
            Vec::new(),
            vec![Arc::from("call")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds one unresolved assignment with caller-selected strict throw or sloppy publication.
    fn unresolved_assignment_module(strictness: FunctionStrictness) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(4, 0);
        builder.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
        builder
            .emit(Opcode::StoreResolvedScope, &[0, 0], span)
            .unwrap();
        builder.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            strictness,
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("missing = 42; missing"),
            Vec::new(),
            vec![Arc::from("missing")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds `new ReferenceError() instanceof ReferenceError` over native construct dispatch.
    fn native_error_constructor_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(4, 0);
        builder.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
        builder.emit(Opcode::Construct, &[1, 0, 0], span).unwrap();
        builder.emit(Opcode::InstanceOf, &[2, 1, 0], span).unwrap();
        builder.emit(Opcode::Return, &[2], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("new ReferenceError() instanceof ReferenceError"),
            Vec::new(),
            vec![Arc::from("ReferenceError")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a method call that stops immediately after pushing its receiver-bearing frame.
    fn method_receiver_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(8, 0);
        entry.emit(Opcode::CreateObject, &[0], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(Opcode::Move, &[2, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[3, 2, 0], span).unwrap();
        entry
            .emit(Opcode::CallWithReceiver, &[4, 2, 0], span)
            .unwrap();
        entry.emit(Opcode::Return, &[4], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("method receiver"),
            Vec::new(),
            vec![Arc::from("method")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Stores a young object through an allocation that forces root tracing before publication.
    fn heap_value_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 0);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::CreateObject, &[1], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        builder.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
        builder.emit(Opcode::Return, &[2], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("heap value property"),
            Vec::new(),
            vec![Arc::from("child")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Stores a young object through a callable's embedded ordinary-property edge.
    fn function_heap_value_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(6, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::CreateObject, &[1], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("function heap property"),
            Vec::new(),
            vec![Arc::from("child")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a same-frame catch range with a pending-exception load at its handler target.
    fn direct_catch_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(10, 1);
        let end = builder.new_label().unwrap();
        let protected_start = builder.emit(Opcode::Nop, &[], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
        builder.emit(Opcode::Throw, &[0], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        let handler = builder.emit(Opcode::LoadException, &[1], span).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::LoadUndefined, &[2], span).unwrap();
        builder.emit(Opcode::Return, &[2], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let mut metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        CompiledModule::new(
            Arc::from("direct catch"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a caller handler around a call whose callee throws through its explicit frame.
    fn cross_frame_catch_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(12, 1);
        let end = entry.new_label().unwrap();
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        let protected_start = entry.emit(Opcode::Nop, &[], span).unwrap();
        entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        entry.emit_jump(end, span).unwrap();
        let handler = entry.emit(Opcode::LoadException, &[2], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        entry.bind_label(end).unwrap();
        entry.emit(Opcode::LoadUndefined, &[3], span).unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
        callee.emit(Opcode::Throw, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let mut entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        entry_metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("cross-frame catch"),
            Vec::new(),
            Vec::new(),
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a constructor that stores one argument on `this` and returns a primitive fallback.
    fn construct_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(6, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
        entry.emit(Opcode::Construct, &[2, 0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[3, 2, 0], span).unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut constructor = BytecodeBuilder::with_capacity(6, 0);
        constructor.emit(Opcode::LoadThis, &[1], span).unwrap();
        constructor.emit(Opcode::SetById, &[1, 0, 0], span).unwrap();
        constructor
            .emit(Opcode::LoadImmediate, &[2, 7], span)
            .unwrap();
        constructor.emit(Opcode::Return, &[2], span).unwrap();
        let (constructor_bytecode, constructor_source_map, constructor_registers) =
            constructor.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let constructor_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: constructor_registers,
                argument_count: 1,
                ..FunctionLayout::default()
            },
            source_map: constructor_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("construct"),
            Vec::new(),
            vec![Arc::from("value")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    constructor_bytecode,
                    constructor_metadata,
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds one default constructor, constructs its receiver, then checks the real prototype link.
    fn instanceof_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(4, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::Construct, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::InstanceOf, &[2, 1, 0], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut constructor = BytecodeBuilder::with_capacity(1, 0);
        constructor
            .emit(Opcode::ReturnUndefined, &[], span)
            .unwrap();
        let (constructor_bytecode, constructor_source_map, constructor_registers) =
            constructor.finish().unwrap();
        CompiledModule::new(
            Arc::from("new Constructor() instanceof Constructor"),
            Vec::new(),
            Vec::new(),
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    constructor_bytecode,
                    FunctionMetadata {
                        layout: FunctionLayout {
                            register_count: constructor_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: constructor_source_map,
                        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds the smallest executable module carrying a chosen entry-state layout contract.
    fn state_module(kind: FunctionKind, layout: FunctionLayout) -> CompiledModule {
        let mut words = encode_instruction(Opcode::LoadUndefined, &[0]).unwrap();
        words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
        CompiledModule::new(
            Arc::from("state"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                Bytecode::from_words(words),
                FunctionMetadata::new(kind, layout),
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }
}
