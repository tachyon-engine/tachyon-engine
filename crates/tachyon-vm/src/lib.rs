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

mod argument_list;
mod array;
mod array_concat;
mod array_copy;
mod array_copy_within;
mod array_fill;
mod array_flat;
mod array_flat_map;
mod array_for_each;
mod array_insert;
mod array_join;
mod array_remove;
mod array_reverse;
mod array_slice;
mod array_splice;
mod array_static;
mod array_to_sorted;
mod async_from_sync_iterator;
mod async_function;
mod async_module;
mod atom;
mod atomics_async;
mod bigint;
mod bound_function;
mod builtins;
mod collection;
mod collection_for_each;
mod conversion;
mod driver;
mod dynamic_function;
mod error;
#[cfg(feature = "opcode-profile")]
mod execution_profile;
mod finalization;
mod finalization_registry;
mod for_in;
mod generator;
mod host;
mod host_agent;
mod interpreter;
mod isolate;
mod iterator;
mod iterator_eager;
mod iterator_helper;
mod iterator_intrinsics;
mod math_conversion;
mod math_sum_precise;
mod module;
mod number;
mod object;
mod promise;
mod promise_capability;
mod promise_combinator_state;
mod promise_state;
mod promise_then;
mod promise_try;
mod property;
mod proxy;
mod realm;
mod regexp;
mod regexp_exec;
mod regexp_flags;
mod regexp_match_all;
mod regexp_replace;
mod regexp_search;
mod runtime;
mod set_methods;
mod string;
mod string_concat;
mod string_from_codes;
mod string_match;
mod string_normalization;
mod string_prototype;
mod string_raw;
mod string_replace_all;
mod string_split;
mod tagged_template;
mod tuning;
mod weak_collection;

pub use atom::{AtomHashSeed, AtomId, AtomTable, AtomTableConfig, AtomTableError, AtomTableStats};

#[cfg(feature = "opcode-profile")]
pub use execution_profile::{ExecutionProfile, OpcodeExecutionCounts};

pub use driver::{PromiseOutcome, VmDriver};
pub use finalization::{
    FinalizationCleanupJob, FinalizationJobQueueStats, FinalizationSafepointError,
    FinalizationSafepointStats,
};
pub use host::{
    AgentBroadcast, AgentBroadcastValue, AgentHostProvider, AtomicsAsyncWait,
    AtomicsAsyncWaitStart, AtomicsWaitLocation, AtomicsWaitResult, AtomicsWaiterProvider,
    HostProviderError, HostProviders, IntlCollatorBackend, IntlCollatorCaseFirst,
    IntlCollatorCreation, IntlCollatorRequest, IntlCollatorResolved, IntlCollatorSensitivity,
    IntlCollatorUsage, IntlFormattedNumberParts, IntlLocaleMatcher, IntlMathematicalValue,
    IntlNumberFormatBackend, IntlNumberFormatCompactDisplay, IntlNumberFormatCreation,
    IntlNumberFormatCurrencyDisplay, IntlNumberFormatCurrencySign, IntlNumberFormatNotation,
    IntlNumberFormatOptions, IntlNumberFormatPartSpan, IntlNumberFormatPartType,
    IntlNumberFormatRequest, IntlNumberFormatResolved, IntlNumberFormatRoundingMode,
    IntlNumberFormatRoundingPriority, IntlNumberFormatSignDisplay, IntlNumberFormatStyle,
    IntlNumberFormatTrailingZeroDisplay, IntlNumberFormatUnitDisplay, IntlNumberFormatUseGrouping,
    IntlProvider, IntlSupportedValuesKey, SharedMemoryId, TimeZoneProvider, WallClockProvider,
};
pub use isolate::Isolate;
pub use module::{
    DynamicImportAttribute, DynamicImportError, DynamicImportRequest, DynamicImportRequestId,
    LoadedModule, ModuleError, ModuleEvaluationError, ModuleExportName, ModuleId, ModuleIdentity,
    ModuleLimits, ModuleLoadError, ModuleLoader, ResolvedModuleRequest,
};
pub use object::{ShapeError, SharedArrayBufferHandle};
pub use runtime::{callable::NativeErrorKind, code::CodeId};
pub use string::{JsString, JsStringView, StringAllocationError, StringRepresentationTag};

use core::{cell::Cell, num::NonZeroU32, ptr::NonNull};

use tachyon_bytecode::{
    BindingLocation, BytecodeConstant, ClassInstanceElementKind, CompiledModule,
    DecodedInstruction, FunctionId, FunctionKind, FunctionLayout, FunctionRole, FunctionStrictness,
    HandlerEntry, HandlerKind, Opcode, RegisterId, VerifiedBytecode, VerifiedInstructionDecoder,
    WordOffset,
};
use tachyon_gc::{
    AllocationSpace, FinalizationRegistration, GcExternalMemory, GcRef, GcType, Heap,
    HeapAllocationError, HeapLimit, HeapReferenceError, KeptObjectError, ManagedAllocationError,
    NoGcBorrowError, PersistentResolveError, PersistentRootError, PersistentRootId, RootError,
    Trace, Tracer, TypeRegistrationError, TypeRegistry,
};
use tachyon_value::Immediate;
pub use tachyon_value::Value;

/// Isolate-local identity for one independent ECMAScript Realm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct RealmId(NonZeroU32);

impl RealmId {
    pub(crate) const MAIN: Self = Self(NonZeroU32::MIN);

    #[inline(always)]
    const fn from_non_zero(value: NonZeroU32) -> Self {
        Self(value)
    }
}

/// Whether the parser proved the syntactic direct-eval form before the runtime identity check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvalKind {
    Direct { strict_caller: bool },
    Indirect,
}

impl EvalKind {
    #[must_use]
    pub const fn inherits_strict(self) -> bool {
        matches!(
            self,
            Self::Direct {
                strict_caller: true
            }
        )
    }
}

/// Host callback used by embedding harnesses to compile and execute eval source in one Realm.
pub type EvalScriptCallback =
    fn(&mut Isolate, RealmId, EvalKind, Value) -> Result<Value, ExecutionError>;

/// Grammar family selected by CreateDynamicFunction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicFunctionKind {
    Ordinary,
    Generator,
    Async,
    AsyncGenerator,
}

/// Frozen UTF-16 inputs for an embedding's dynamic-function compiler.
#[derive(Debug)]
pub struct DynamicFunctionSource {
    pub parameters: Box<[Box<[u16]>]>,
    pub body: Box<[u16]>,
}

/// Host callback used to compile dynamic `Function` constructor source in one Realm.
pub type DynamicFunctionCallback = fn(
    &mut Isolate,
    RealmId,
    DynamicFunctionKind,
    DynamicFunctionSource,
) -> Result<Value, ExecutionError>;

#[derive(Clone, Copy)]
pub(crate) enum IntrinsicPrototypeKind {
    Object,
    Array,
    Boolean,
    Date,
    IntlCollator,
    IntlNumberFormat,
    SignalState,
    SignalComputed,
    SignalWatcher,
    String,
    Promise,
    Function,
    GeneratorFunction,
    AsyncFunction,
    AsyncGeneratorFunction,
    Generator,
    AsyncGenerator,
}

use argument_list::{ArgumentListOperation, PendingArgumentList};
use array::{ArrayElements, ArrayObject, MAX_SAFE_INTEGER, safe_integer_property_index};
use array_concat::PendingArrayConcat;
use array_copy::{ArrayCopyKind, PendingArrayCopy};
use array_copy_within::PendingArrayCopyWithin;
use array_fill::PendingArrayFill;
use array_flat::PendingArrayFlat;
use array_flat_map::PendingArrayFlatMap;
use array_insert::PendingArrayInsert;
use array_join::PendingArrayJoin;
use array_remove::PendingArrayRemove;
use array_reverse::PendingArrayReverse;
use array_slice::PendingArraySlice;
use array_splice::PendingArraySplice;
use array_static::PendingArrayStatic;
use array_to_sorted::PendingArrayToSorted;
use async_from_sync_iterator::AsyncFromSyncIteratorObject;
use bigint::{BigIntValue, small_bigint_binary_hot, small_bigint_not_hot};
use bound_function::BoundFunctionData;
use builtins::object::PendingGetOwnPropertyDescriptors;
use builtins::signals::{ComputedSignal, SignalRuntime, StateSignal, WatcherSignal};
use builtins::{
    PendingDateNumericArguments, PendingIntlCollator, PendingIntlNumberFormat,
    PendingJsonStringify, math_variadic_add, math_variadic_finish, math_variadic_initial,
};
use collection::{
    CollectionInitializerKind, MapObject, OrderedCollection, PendingCollectionForEach,
    PendingCollectionInitializer, PendingMapGetOrInsertComputed, SetObject,
};
use conversion::{
    PendingNativePropertyKey, boolean_value, is_non_string_truthy, is_nullish, numeric_binary,
    numeric_binary_hot, numeric_binary_operation, numeric_bitwise_not, numeric_negate,
    numeric_relational, numeric_relational_hot, numeric_value, safe_integer_value,
    strict_equal_hot,
};
use dynamic_function::PendingDynamicFunction;
use error::ErrorObject;
use finalization_registry::{FinalizationCell, FinalizationRegistryObject};
use for_in::{ForInAllocationError, ForInIterator, ForInKeySet};
#[cfg(test)]
use interpreter::execute_verified_hot_instruction;
use iterator::{
    ArrayIterationKind, ArrayIteratorNextAction, ArrayIteratorObject, CollectionIterationKind,
    CollectionIteratorObject,
};
use iterator_eager::{IteratorEagerKind, IteratorEagerOperation};
use iterator_helper::{IteratorHelperObject, WrapForValidIteratorObject};
use math_conversion::PendingMathOperation;
use math_sum_precise::ExactSumAccumulator;
use object::{
    ArgumentsObject, BigIntObject, BooleanObject, DateObject, IntlCollatorBackendPayload,
    IntlCollatorObject, IntlCollatorResolvedOptions, IntlNumberFormatObject,
    IntlNumberFormatPayload, NumberObject, OrdinaryObject, PropertyAttributes, PropertyKey,
    PropertyKind, PropertyLookup, PropertyStorage, RegExpObject, ShapeId, ShapeTable,
    SharedArrayBufferBacking, SharedArrayBufferData, StringObject, SymbolId, SymbolObject,
    SymbolPropertyKey,
};
use promise_combinator_state::{
    PendingPromiseCombinator, PromiseCombinatorElement, PromiseCombinatorKind,
    PromiseCombinatorStage,
};
use promise_state::{
    GenericPromiseCapabilityRoots, PromiseCapability, PromiseCapabilityRoots, PromiseJob,
    PromiseJobQueue, PromiseObject, PromiseReaction, PromiseReactionRoots, PromiseResolutionCell,
    PromiseState,
};
use property::copy::{ExclusionList, PendingCopyDataProperties, PendingObjectAssign};
use property::{
    PendingDefineProperties, PendingPropertyDescriptor, PropertyRead, PropertyReadResolution,
    PropertyWrite, PropertyWriteResolution,
};
use proxy::PendingProxyOwnKeys;
use proxy::{
    PROXY_ACTIVE_OBJECT, PROXY_DEFINE_HANDLER, PROXY_DELETE_ACTIVE, PROXY_GET_ACTIVE,
    PendingProxyDefine, ProxyObject,
};
use regexp_match_all::RegExpStringIteratorObject;
use regexp_replace::PendingRegExpReplace;
#[cfg(feature = "opcode-profile")]
use runtime::code::is_conditional_branch;
use runtime::{
    agent::{AgentState, RegisteredSymbol, WellKnownSymbolId},
    callable::{
        AccessorPair, AccessorPropertyDescriptor, AtomicsFunction, BoundFunctionSnapshot, CallSite,
        DataPropertyDescriptor, DateUtcField, DateUtcSetter, ErrorIntrinsics,
        FunctionAuxiliaryEdge, FunctionExecutable, FunctionObject, GenericPropertyDescriptor,
        GlobalNumberFunction, GlobalUriFunction, HostAgentFunction, IntrinsicPropertyAtoms,
        MathFunction, NativeCallState, NativeFunction, ObjectReceiver, PropertyDescriptor,
        RealmIntrinsicAtoms, RegExpGetter, ResolvedCallTarget, SymbolValue, VmTypes,
        execution_error_kind,
    },
    class::{
        ClassConstructorData, ClassInstanceElementPlan, ClassInstanceElementRecord,
        PendingInstanceElements,
    },
    code::{BytecodeCursor, HotControl, LoadedCode, RegisterWindow, ScopeResolution},
    completion::{CompletionKind, CompletionRecord, CompletionStackError},
    environment::{
        BindingState, Environment, EnvironmentAccessError, EnvironmentKind, EnvironmentOwner,
    },
    fiber::{
        ActiveHandler, ArrayAllocationRoots, ArrayConcatStage, ArrayCopyStage,
        ArrayCopyWithinStage, ArrayFillStage, ArrayFlatMapStage, ArrayFlatStage, ArrayForEachStage,
        ArrayInsertStage, ArrayJoinStage, ArrayRemoveStage, ArrayReverseStage, ArraySliceStage,
        ArraySpliceStage, ArrayStaticStage, ArrayToSortedStage, AsyncFromSyncIteratorStage,
        BuiltinPropertyKeyConsumer, ClassActivation, CodeLoadRoots, CollectionInitializerStage,
        CollectionIteratorCloseStage, ConstructReceiverRoots, ConversionCallbackStage,
        ConversionConsumer, ConversionContinuation, ConversionNativeFunction,
        CopyDataPropertiesStage, DateToJsonStage, DefinePropertiesStage, ErrorConstructorStage,
        ErrorStackSetterStage, ErrorToStringStage, EvalVarEnvironment, Fiber, Frame,
        GetOwnPropertyDescriptorsStage, InstanceElementStage, InstanceOfStage, IntlCollatorStage,
        IntlLocaleListStage, IntlNumberFormatLegacyStage, IntlNumberFormatStage,
        IteratorEagerStage, IteratorFromStage, IteratorHelperStage, IteratorPrototypeSetterKey,
        IteratorPrototypeSetterStage, JsonStringifyStage, MathSumPreciseStage, NativeContinuation,
        NativeContinuationKind, NativeContinuationSite, ObjectLookupAccessorStage,
        ObjectToLocaleStringStage, PreferredType, PromiseCatchStage, PromiseFinallyMethodStage,
        PromiseResolutionMode, PromiseStaticResolveStage, PromiseThenStage, PropertyCallbackMode,
        PropertyMutationRoots, PropertyWriteMode, PrototypeInitializationRoots, ProxyCallStage,
        ProxyContinuationStage, ProxyDefineMode, ProxyDefineStage, ProxyDeleteMode,
        ProxyDeleteStage, ProxyGetOwnMode, ProxyGetOwnStage, ProxyGetStage, ProxyHasStage,
        ProxyInternalMethod, ProxyOwnKeysMode, ProxyOwnKeysStage, ProxySetMode,
        ProxySetPrototypeMode, ProxySetPrototypeStage, ProxySetStage, RegExpSearchStage,
        RegExpStringIteratorStage, RegExpTestStage, SetOperationStage, SignalStateStage,
        StringMatchStage, StringPrototypeOperation, StringPrototypeStage, StringRawStage,
        StringReplaceAllStage, StringSplitStage, SymbolAllocationRoots, ToPrimitiveStage,
        TypedArrayConstructionStage, TypedArraySetStage, TypedArraySliceStage,
        TypedArraySubarrayStage, TypedArrayTransformStage, VmRoots, WrapForValidIteratorStage,
        next_to_primitive_stage,
    },
    realm::{
        GlobalLexicalSlotId, GlobalSlotId, IntrinsicSlotId, PrimitiveHintStrings, Realm,
        TypeofStrings,
    },
};
use set_methods::{PendingSetOperation, SetOperationKind};
use string_concat::PendingStringConcat;
use string_from_codes::PendingStringFromCodes;
use string_raw::PendingStringRaw;
use weak_collection::{WeakCollection, WeakMapObject, WeakRefObject, WeakSetObject};

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
    module_limits: ModuleLimits,
}

impl IsolateConfig {
    #[must_use]
    pub const fn new(
        atom_table: AtomTableConfig,
        heap_limit: HeapLimit,
        stack_limits: StackLimits,
        realm_limits: RealmLimits,
    ) -> Self {
        let module_limits = ModuleLimits::new(
            realm_limits.max_loaded_modules,
            realm_limits.max_global_bindings,
            realm_limits
                .max_loaded_modules
                .saturating_mul(tuning::modules::DEFAULT_EDGE_CAPACITY_PER_MODULE),
        );
        Self {
            atom_table,
            heap_limit,
            stack_limits,
            realm_limits,
            module_limits,
        }
    }

    /// Overrides graph, binding-cell, and traversal hard limits for module-heavy embeddings.
    #[must_use]
    pub const fn with_module_limits(mut self, module_limits: ModuleLimits) -> Self {
        self.module_limits = module_limits;
        self
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
    max_completions: u32,
}

impl StackLimits {
    #[must_use]
    pub const fn new(max_frames: u32, max_registers: u32) -> Self {
        Self {
            max_frames,
            max_registers,
            max_completions: max_registers,
        }
    }

    /// Overrides completion storage independently when finally/native nesting needs a tighter cap.
    #[must_use]
    pub const fn with_max_completions(mut self, max_completions: u32) -> Self {
        self.max_completions = max_completions;
        self
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
    DriverBusy,
    MissingWallClockProvider,
    WallClockProvider(HostProviderError),
    MissingTimeZoneProvider,
    TimeZoneProvider(HostProviderError),
    MissingIntlProvider,
    IntlProvider(HostProviderError),
    MissingAtomicsWaiterProvider,
    AtomicsWaiterProvider(HostProviderError),
    MissingAgentHostProvider,
    AgentHostProvider(HostProviderError),
    MissingEntryFunction(FunctionId),
    MissingFunctionSource { code: CodeId, function: FunctionId },
    RegisterWindowTooLarge(u32),
    HandlerStackTooLarge(u32),
    CompletionStackTooLarge(u32),
    FrameAllocationFailed,
    RegisterAllocationFailed,
    EnvironmentStorageAllocationFailed,
    HandlerAllocationFailed,
    CompletionAllocationFailed,
    CompletionStackLimit { limit: u32, requested: u32 },
    DecodeInvariant(WordOffset),
    UnsupportedOpcode(Opcode),
    UnsupportedConstant(u32),
    InvalidRegister(RegisterId),
    NonCallable(Value),
    GeneratorBrand(Value),
    GeneratorExecuting,
    UnsupportedGeneratorYieldResume,
    GeneratorArgumentAllocationFailed,
    UnsupportedAsyncFunctionResume,
    AsyncFunctionArgumentAllocationFailed,
    NonConstructor(Value),
    ArrayReduceEmpty,
    ClassConstructorCalledWithoutNew(Value),
    UninitializedThis,
    SuperAlreadyCalled,
    InvalidDerivedConstructorReturn(Value),
    InvalidInstanceofPrototype(Value),
    HeapAllocation(ManagedAllocationError),
    HeapReference(HeapReferenceError),
    KeptObject(KeptObjectError),
    Root(RootError),
    PersistentRoot(PersistentRootError),
    PersistentResolve(PersistentResolveError),
    NoGcBorrow(NoGcBorrowError),
    MissingPendingException,
    MissingNativeContinuation,
    FinalizationCleanupReentrant,
    FinalizationJobQueueAllocationFailed,
    HostThrown(Value),
    MissingCompletionRecord,
    UnsupportedExceptionHandler(HandlerKind),
    CallStackLimit { limit: u32 },
    RegisterStackLimit { limit: u32, requested: u32 },
    LoadedModuleLimit { limit: u32 },
    RealmLimit { limit: u32 },
    LoadedCodeAllocationFailed,
    Module(ModuleError),
    ScopeNameAllocationFailed,
    ScopeNameAtom(AtomTableError),
    ScopeNameString(StringAllocationError),
    ConstantValueAllocationFailed,
    ClassFieldAllocationFailed,
    InvalidClassFieldPlan,
    PrivateBrandCheckFailed(Value),
    PrivatePropertyKeyEscaped,
    ConstantString(StringAllocationError),
    FunctionSourceString(StringAllocationError),
    PropertyKeyAtom(AtomTableError),
    PropertyKeyString(StringAllocationError),
    UnsupportedPropertyKey(Value),
    UnsupportedNumberConversion(Value),
    InvalidSetSize(Value),
    NegativeSetSize(Value),
    TypedArrayContentTypeMismatch,
    AtomicsWaitRequiresSharedArrayBuffer,
    AtomicsWaitCannotSuspend,
    InvalidNumberRadix(Value),
    InvalidNumberPrecision(Value),
    InvalidDateValue,
    InvalidDatePrimitiveHint(Value),
    NumberFormatBufferExhausted,
    NumberFormatInvalidDigit,
    NumberStringAllocationFailed,
    MathArgumentAllocationFailed,
    StringBufferAllocationFailed,
    InvalidStringLength,
    InvalidStringRepeatCount(Value),
    InvalidNormalizationForm,
    InvalidLanguageTag,
    InvalidIntlSupportedValuesKey,
    InvalidIntlCollatorOption,
    InvalidIntlNumberFormatOption,
    MissingIntlNumberFormatCurrency,
    MissingIntlNumberFormatUnit,
    InvalidIntlNumberFormatRoundingIncrementCombination,
    InvalidLocaleListElement(Value),
    IncompatibleIntlCollatorReceiver(Value),
    IncompatibleIntlNumberFormatReceiver(Value),
    InvalidUriEncoding,
    UnsupportedTypeof(Value),
    InvalidCode(CodeId),
    InvalidScopeName { code: CodeId, scope_name: u32 },
    InvalidAtom(AtomId),
    MissingEnvironment,
    MissingModuleContext,
    InvalidEnvironmentSlot { depth: u32, slot: u32 },
    UninitializedEnvironmentBinding { depth: u32, slot: u32 },
    ImmutableEnvironmentBinding { depth: u32, slot: u32 },
    EnvironmentBindingAlreadyInitialized { depth: u32, slot: u32 },
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
    CollectionStorageAllocationFailed,
    UnsupportedCollectionInitializer,
    IncompatibleCollectionReceiver(Value),
    IncompatibleFinalizationRegistryReceiver(Value),
    InvalidFinalizationRegistration(Value),
    ExclusionListAllocationFailed,
    ExclusionListCapacityExceeded,
    InvalidExclusionList(Value),
    CopyDataPropertiesAllocationFailed,
    BoundArgumentAllocationFailed,
    BoundArgumentCountOverflow,
    BoundNameAllocationFailed,
    SymbolIdExhausted,
    SymbolRegistryAllocationFailed,
    ArrayLengthOverflow,
    InvalidArrayLength,
    InvalidBigIntLiteral,
    InvalidBigIntNumber(Value),
    InvalidBigIntValue(Value),
    UnsupportedBigIntConversion(Value),
    BigIntAllocationFailed,
    BigIntDivisionByZero,
    BigIntNegativeExponent,
    BigIntResultTooLarge,
    BigIntUnsignedRightShift,
    BigIntMixedTypes,
    TypedArraySpeciesResultTooShort,
    DetachedArrayBuffer,
    FixedLengthSharedArrayBuffer,
    OutOfBoundsTypedArray,
    OutOfBoundsDataView,
    NonResizableArrayBuffer,
    TypedArraySetAllocationFailed,
    OwnPropertyKeyAllocationFailed,
    ForInKeyAllocationFailed,
    InvalidForInIterator(Value),
    UnsupportedErrorMessage(Value),
    UnsupportedStringValue(Value),
    UnsupportedPrimitiveStringConversion(Value),
    InvalidJsonText,
    JsonSerializationDepthExceeded,
    InvalidEvalSource,
    InvalidRegExpFlags,
    InvalidRegExpPattern,
    RegExpMatchAllRequiresGlobal,
    InvalidJsonCircularStructure,
    UnsupportedDynamicFunctionConstructor,
    RealmConstructionRootAllocationFailed,
    ProxyConstructorRequiresNew,
    ProxyRevoked,
    ProxyInvariantViolation,
    NonExtensibleObject(Value),
    ReadOnlyProperty(Value),
    InvalidPropertyDescriptor(Value),
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
    ModuleGraph(module::ModuleError),
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

#[cfg(test)]
mod tests;
