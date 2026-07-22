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
mod array_for_each;
mod atom;
mod bound_function;
mod builtins;
mod collection;
mod collection_for_each;
mod conversion;
mod error;
#[cfg(feature = "opcode-profile")]
mod execution_profile;
mod finalization;
mod for_in;
mod interpreter;
mod isolate;
mod iterator;
mod number;
mod object;
mod promise;
mod promise_capability;
mod promise_state;
mod promise_then;
mod property;
mod proxy;
mod realm;
mod regexp;
mod runtime;
mod string;
mod tuning;
mod weak_collection;

pub use atom::{AtomHashSeed, AtomId, AtomTable, AtomTableConfig, AtomTableError, AtomTableStats};

#[cfg(feature = "opcode-profile")]
pub use execution_profile::{ExecutionProfile, OpcodeExecutionCounts};

pub use finalization::{
    FinalizationCleanupJob, FinalizationJobQueueStats, FinalizationSafepointError,
    FinalizationSafepointStats,
};
pub use isolate::Isolate;
pub use object::ShapeError;
pub use runtime::{callable::NativeErrorKind, code::CodeId};
pub use string::{JsString, JsStringView, StringAllocationError, StringRepresentationTag};

use core::{cell::Cell, num::NonZeroU32, ptr::NonNull};

use tachyon_bytecode::{
    BytecodeConstant, ClassInstanceElementKind, CompiledModule, DecodedInstruction, FunctionId,
    FunctionKind, FunctionLayout, FunctionStrictness, HandlerEntry, HandlerKind, Opcode,
    RegisterId, VerifiedBytecode, VerifiedInstructionDecoder, WordOffset,
};
use tachyon_gc::{
    AllocationSpace, GcExternalMemory, GcRef, GcType, Heap, HeapAllocationError, HeapLimit,
    HeapReferenceError, ManagedAllocationError, NoGcBorrowError, RootError, Trace, Tracer,
    TypeRegistrationError, TypeRegistry,
};
use tachyon_value::{Immediate, Value};

use argument_list::{ArgumentListOperation, PendingArgumentList};
use array::{ArrayObject, MAX_SAFE_INTEGER};
use bound_function::BoundFunctionData;
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
use error::ErrorObject;
use for_in::{ForInAllocationError, ForInIterator, ForInKeySet};
#[cfg(test)]
use interpreter::execute_verified_hot_instruction;
use iterator::{
    ArrayIterationKind, ArrayIteratorNextAction, ArrayIteratorObject, CollectionIterationKind,
    CollectionIteratorObject,
};
use object::{
    ArgumentsObject, BooleanObject, NumberObject, OrdinaryObject, PropertyAttributes, PropertyKey,
    PropertyKind, PropertyLookup, PropertyStorage, RegExpObject, ShapeId, ShapeTable, StringObject,
    SymbolId, SymbolObject, SymbolPropertyKey,
};
use promise_state::{
    GenericPromiseCapabilityRoots, PromiseCapability, PromiseCapabilityRoots, PromiseJob,
    PromiseJobQueue, PromiseObject, PromiseReaction, PromiseReactionRoots, PromiseResolutionCell,
    PromiseState,
};
use property::copy::{ExclusionList, PendingCopyDataProperties};
use property::{PendingPropertyDescriptor, PropertyRead, PropertyReadResolution, PropertyWrite};
use proxy::{
    PROXY_ACTIVE_OBJECT, PROXY_DEFINE_HANDLER, PROXY_DELETE_ACTIVE, PROXY_GET_ACTIVE,
    PendingProxyDefine, ProxyObject,
};
#[cfg(feature = "opcode-profile")]
use runtime::code::is_conditional_branch;
use runtime::{
    callable::{
        AccessorPair, AccessorPropertyDescriptor, BoundFunctionSnapshot, CallSite,
        DataPropertyDescriptor, ErrorIntrinsics, FlatWork, FunctionExecutable, FunctionObject,
        GenericPropertyDescriptor, GlobalNumberFunction, IntrinsicPropertyAtoms, MathFunction,
        NativeCallState, NativeFunction, ObjectReceiver, PropertyDescriptor, RealmIntrinsicAtoms,
        ResolvedCallTarget, SymbolValue, VmTypes, execution_error_kind,
    },
    class::{
        ClassConstructorData, ClassInstanceElementPlan, ClassInstanceElementRecord,
        PendingInstanceElements,
    },
    code::{BytecodeCursor, HotControl, LoadedCode, RegisterWindow, ScopeResolution},
    completion::{CompletionKind, CompletionRecord, CompletionStackError},
    environment::{BindingState, Environment, EnvironmentAccessError, EnvironmentKind},
    fiber::{
        ActiveHandler, ArrayAllocationRoots, ArrayForEachStage, BuiltinPropertyKeyConsumer,
        ClassActivation, CodeLoadRoots, CollectionInitializerStage, ConversionCallbackStage,
        ConversionConsumer, ConversionContinuation, ConversionNativeFunction,
        ErrorConstructorStage, ErrorToStringStage, Fiber, Frame, InstanceElementStage,
        NativeContinuation, NativeContinuationKind, NativeContinuationSite, PreferredType,
        PromiseResolutionMode, PromiseStaticResolveStage, PromiseThenStage, PropertyCallbackMode,
        PropertyMutationRoots, PropertyWriteMode, PrototypeInitializationRoots,
        ProxyContinuationStage, ProxyDefineMode, ProxyDefineStage, ProxyDeleteMode,
        ProxyDeleteStage, ProxyGetOwnMode, ProxyGetOwnStage, ProxyGetStage, ProxyHasStage,
        ProxyInternalMethod, ProxySetPrototypeMode, ProxySetPrototypeStage, SymbolAllocationRoots,
        ToPrimitiveStage, VmRoots, next_to_primitive_stage,
    },
    realm::{
        GlobalLexicalSlotId, GlobalSlotId, IntrinsicSlotId, PrimitiveHintStrings, Realm,
        TypeofStrings,
    },
};
use weak_collection::{WeakCollection, WeakMapObject, WeakSetObject};

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
    MissingEntryFunction(FunctionId),
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
    NonConstructor(Value),
    ClassConstructorCalledWithoutNew(Value),
    UninitializedThis,
    SuperAlreadyCalled,
    InvalidDerivedConstructorReturn(Value),
    InvalidInstanceofPrototype(Value),
    HeapAllocation(ManagedAllocationError),
    HeapReference(HeapReferenceError),
    Root(RootError),
    NoGcBorrow(NoGcBorrowError),
    MissingPendingException,
    MissingNativeContinuation,
    MissingCompletionRecord,
    UnsupportedExceptionHandler(HandlerKind),
    CallStackLimit { limit: u32 },
    RegisterStackLimit { limit: u32, requested: u32 },
    LoadedModuleLimit { limit: u32 },
    LoadedCodeAllocationFailed,
    ScopeNameAllocationFailed,
    ScopeNameAtom(AtomTableError),
    ScopeNameString(StringAllocationError),
    ConstantValueAllocationFailed,
    ClassFieldAllocationFailed,
    InvalidClassFieldPlan,
    PrivateBrandCheckFailed(Value),
    PrivatePropertyKeyEscaped,
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
    InvalidStringLength,
    InvalidStringRepeatCount(Value),
    UnsupportedTypeof(Value),
    InvalidCode(CodeId),
    InvalidScopeName { code: CodeId, scope_name: u32 },
    InvalidAtom(AtomId),
    MissingEnvironment,
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
    OwnPropertyKeyAllocationFailed,
    ForInKeyAllocationFailed,
    InvalidForInIterator(Value),
    UnsupportedErrorMessage(Value),
    UnsupportedStringValue(Value),
    UnsupportedPrimitiveStringConversion(Value),
    InvalidJsonText,
    InvalidRegExpFlags,
    InvalidRegExpPattern,
    InvalidJsonCircularStructure,
    UnsupportedDynamicFunctionConstructor,
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
