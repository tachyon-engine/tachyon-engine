//! Explicit fiber, frame, continuation, and GC root state.

use super::super::*;
use super::completion::CompletionStack;

pub(crate) struct VmRoots<'a> {
    pub(crate) fiber: &'a mut Fiber,
    pub(crate) finalization_jobs: &'a mut finalization::FinalizationJobs,
    pub(crate) promise_jobs: &'a mut PromiseJobQueue,
    pub(crate) realm: &'a mut Realm,
    pub(crate) loaded_code: &'a mut Vec<LoadedCode>,
}

pub(crate) struct PropertyMutationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) receiver: Value,
    pub(crate) value: Value,
    pub(crate) symbol_key: Option<Value>,
}

pub(crate) struct SymbolAllocationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) description: Option<Value>,
}

pub(crate) struct PrototypeInitializationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) function: Value,
    pub(crate) object_prototype: Value,
}

pub(crate) struct ArrayAllocationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) prototype: Value,
}

impl Trace for PropertyMutationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
        self.value.trace(tracer);
        self.symbol_key.trace(tracer);
    }
}

impl Trace for SymbolAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.description.trace(tracer);
    }
}

impl Trace for PrototypeInitializationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.function.trace(tracer);
        self.object_prototype.trace(tracer);
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
        self.promise_jobs.trace(tracer);
        self.realm.trace(tracer);
        for code in self.loaded_code.iter_mut() {
            code.trace(tracer);
        }
    }
}

pub(crate) struct CodeLoadRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) constant_values: &'a mut Vec<Option<Value>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeContinuationSite {
    pub(crate) caller_base: u32,
    pub(crate) destination: u32,
    pub(crate) call_site: WordOffset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToPrimitiveStage {
    Exotic,
    ValueOf,
    ToString,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreferredType {
    Default,
    String,
    Number,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConversionCallbackStage {
    Getter,
    MethodCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum BuiltinPropertyKeyConsumer {
    DefineProperty,
    ReflectDefineProperty,
    GetOwnPropertyDescriptor,
    ReflectGetOwnPropertyDescriptor,
    HasOwnProperty,
    PropertyIsEnumerable,
    DefineGetter,
    DefineSetter,
    LookupGetter,
    LookupSetter,
    HasOwn,
    ReflectDeleteProperty,
    ReflectHas,
    ReflectGet,
    ReflectSet,
    ObjectFromEntries,
    ObjectGroupBy,
}

const _: [(); 1] = [(); core::mem::size_of::<BuiltinPropertyKeyConsumer>()];

/// Compact identity for the small native subset that can suspend during primitive conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ConversionNativeFunction {
    StringConstructor,
    NumberConstructor,
    NumberToExponential,
    NumberToFixed,
    NumberToPrecision,
    NumberToString,
    StringToLowerCase,
    StringToUpperCase,
    StringToLocaleLowerCase,
    StringToLocaleUpperCase,
    GlobalIsFinite,
    GlobalIsNaN,
    GlobalParseFloat,
    GlobalParseInt,
    GlobalDecodeUri,
    GlobalDecodeUriComponent,
    GlobalEncodeUri,
    GlobalEncodeUriComponent,
}

impl ConversionNativeFunction {
    pub(crate) const fn from_native(native: NativeFunction) -> Option<Self> {
        match native {
            NativeFunction::StringConstructor => Some(Self::StringConstructor),
            NativeFunction::NumberConstructor => Some(Self::NumberConstructor),
            NativeFunction::NumberToExponential => Some(Self::NumberToExponential),
            NativeFunction::NumberToFixed => Some(Self::NumberToFixed),
            NativeFunction::NumberToPrecision => Some(Self::NumberToPrecision),
            NativeFunction::NumberToString => Some(Self::NumberToString),
            NativeFunction::StringToLowerCase => Some(Self::StringToLowerCase),
            NativeFunction::StringToUpperCase => Some(Self::StringToUpperCase),
            NativeFunction::StringToLocaleLowerCase => Some(Self::StringToLocaleLowerCase),
            NativeFunction::StringToLocaleUpperCase => Some(Self::StringToLocaleUpperCase),
            NativeFunction::GlobalIsFinite => Some(Self::GlobalIsFinite),
            NativeFunction::GlobalIsNaN => Some(Self::GlobalIsNaN),
            NativeFunction::GlobalParseFloat => Some(Self::GlobalParseFloat),
            NativeFunction::GlobalParseInt => Some(Self::GlobalParseInt),
            NativeFunction::GlobalDecodeUri => Some(Self::GlobalDecodeUri),
            NativeFunction::GlobalDecodeUriComponent => Some(Self::GlobalDecodeUriComponent),
            NativeFunction::GlobalEncodeUri => Some(Self::GlobalEncodeUri),
            NativeFunction::GlobalEncodeUriComponent => Some(Self::GlobalEncodeUriComponent),
            _ => None,
        }
    }

    pub(crate) const fn native(self) -> NativeFunction {
        match self {
            Self::StringConstructor => NativeFunction::StringConstructor,
            Self::NumberConstructor => NativeFunction::NumberConstructor,
            Self::NumberToExponential => NativeFunction::NumberToExponential,
            Self::NumberToFixed => NativeFunction::NumberToFixed,
            Self::NumberToPrecision => NativeFunction::NumberToPrecision,
            Self::NumberToString => NativeFunction::NumberToString,
            Self::StringToLowerCase => NativeFunction::StringToLowerCase,
            Self::StringToUpperCase => NativeFunction::StringToUpperCase,
            Self::StringToLocaleLowerCase => NativeFunction::StringToLocaleLowerCase,
            Self::StringToLocaleUpperCase => NativeFunction::StringToLocaleUpperCase,
            Self::GlobalIsFinite => NativeFunction::GlobalIsFinite,
            Self::GlobalIsNaN => NativeFunction::GlobalIsNaN,
            Self::GlobalParseFloat => NativeFunction::GlobalParseFloat,
            Self::GlobalParseInt => NativeFunction::GlobalParseInt,
            Self::GlobalDecodeUri => NativeFunction::GlobalDecodeUri,
            Self::GlobalDecodeUriComponent => NativeFunction::GlobalDecodeUriComponent,
            Self::GlobalEncodeUri => NativeFunction::GlobalEncodeUri,
            Self::GlobalEncodeUriComponent => NativeFunction::GlobalEncodeUriComponent,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConversionConsumer {
    NativeCall(ConversionNativeFunction),
    NativeConstruct(ConversionNativeFunction),
    ToNumber,
    Negate,
    BitwiseNot,
    BinaryLeft(Opcode),
    BinaryRight(Opcode),
    AddLeft,
    AddRight,
    RelationalLeft(Opcode),
    RelationalRight(Opcode),
    Equality(Opcode),
    ToPropertyKey,
    BuiltinPropertyKey(BuiltinPropertyKeyConsumer),
    ErrorConstructorMessage,
    ErrorToStringName,
    ErrorToStringMessage,
}

impl ConversionConsumer {
    #[inline]
    pub(crate) const fn native(self) -> Option<NativeFunction> {
        match self {
            Self::NativeCall(native) | Self::NativeConstruct(native) => Some(native.native()),
            Self::ToNumber
            | Self::Negate
            | Self::BitwiseNot
            | Self::BinaryLeft(_)
            | Self::BinaryRight(_)
            | Self::AddLeft
            | Self::AddRight
            | Self::RelationalLeft(_)
            | Self::RelationalRight(_)
            | Self::Equality(_)
            | Self::ToPropertyKey
            | Self::BuiltinPropertyKey(_)
            | Self::ErrorConstructorMessage
            | Self::ErrorToStringName
            | Self::ErrorToStringMessage => None,
        }
    }

    #[inline]
    pub(crate) const fn uses_string_hint(self) -> bool {
        matches!(
            self,
            Self::NativeCall(ConversionNativeFunction::StringConstructor)
                | Self::NativeCall(ConversionNativeFunction::StringToLowerCase)
                | Self::NativeCall(ConversionNativeFunction::StringToUpperCase)
                | Self::NativeCall(ConversionNativeFunction::StringToLocaleLowerCase)
                | Self::NativeCall(ConversionNativeFunction::StringToLocaleUpperCase)
                | Self::NativeCall(ConversionNativeFunction::GlobalParseFloat)
                | Self::NativeCall(ConversionNativeFunction::GlobalParseInt)
                | Self::NativeCall(ConversionNativeFunction::GlobalDecodeUri)
                | Self::NativeCall(ConversionNativeFunction::GlobalDecodeUriComponent)
                | Self::NativeCall(ConversionNativeFunction::GlobalEncodeUri)
                | Self::NativeCall(ConversionNativeFunction::GlobalEncodeUriComponent)
                | Self::ToPropertyKey
                | Self::BuiltinPropertyKey(_)
                | Self::ErrorConstructorMessage
                | Self::ErrorToStringName
                | Self::ErrorToStringMessage
        )
    }

    #[inline]
    pub(crate) const fn preferred_type(self) -> PreferredType {
        if self.uses_string_hint() {
            PreferredType::String
        } else if matches!(self, Self::AddLeft | Self::AddRight | Self::Equality(_)) {
            PreferredType::Default
        } else {
            PreferredType::Number
        }
    }

    #[inline]
    pub(crate) const fn is_resumable_operation(self) -> bool {
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
                | Self::Equality(_)
                | Self::ToPropertyKey
                | Self::BuiltinPropertyKey(_)
                | Self::ErrorConstructorMessage
                | Self::ErrorToStringName
                | Self::ErrorToStringMessage
        )
    }
}

#[inline]
pub(crate) fn next_to_primitive_stage(
    consumer: ConversionConsumer,
    stage: ToPrimitiveStage,
) -> Option<ToPrimitiveStage> {
    if stage == ToPrimitiveStage::Exotic {
        return Some(if consumer.uses_string_hint() {
            ToPrimitiveStage::ToString
        } else {
            ToPrimitiveStage::ValueOf
        });
    }
    if consumer.uses_string_hint() {
        match stage {
            ToPrimitiveStage::Exotic => unreachable!("exotic stage returns before hint ordering"),
            ToPrimitiveStage::ToString => Some(ToPrimitiveStage::ValueOf),
            ToPrimitiveStage::ValueOf => None,
        }
    } else {
        match stage {
            ToPrimitiveStage::Exotic => unreachable!("exotic stage returns before hint ordering"),
            ToPrimitiveStage::ValueOf => Some(ToPrimitiveStage::ToString),
            ToPrimitiveStage::ToString => None,
        }
    }
}

/// Resumable ordinary conversion state retained while one JavaScript method callback executes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConversionContinuation {
    pub(crate) site: NativeContinuationSite,
    pub(crate) consumer: ConversionConsumer,
    pub(crate) receiver: Value,
    pub(crate) object: Value,
    pub(crate) stage: ToPrimitiveStage,
    pub(crate) callback_stage: ConversionCallbackStage,
}

/// Typed callback work owned by a JavaScript frame instead of the Rust call stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PropertyCallbackMode {
    Ordinary,
    Descriptor,
    ArrayIteratorLength,
    ArrayIteratorElement,
    ArgumentList,
    CopyDataProperties,
    DefineProperties,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyOwnKeysMode {
    Internal,
    Reflect,
    Names,
    Symbols,
    EnumerableNames,
    IntegritySealed,
    IntegrityFrozen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyOwnKeysStage {
    TrapGetter,
    TrapCall,
    LengthGet,
    ElementGet,
    TargetOwnKeys,
    IntegrityExtensible,
    IntegrityDescriptor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PropertyWriteMode {
    Assignment,
    Reflect,
    ObjectAssign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum CopyDataPropertiesStage {
    OwnKeys,
    Enumerable,
    Get,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum DefinePropertiesStage {
    OwnKeys,
    Enumerable,
    Get,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum GetOwnPropertyDescriptorsStage {
    OwnKeys,
    Descriptor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxySetMode {
    Assignment,
    Reflect,
    ObjectAssign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxySetStage {
    TrapGetter,
    TrapCall,
    ReceiverGetOwn,
    ReceiverDefine,
}

/// Identifies the observable caller that must resume after `Get(resolution, "then")`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseResolutionMode {
    ResolverCall,
    Reaction,
    StaticResolve,
}

/// One observable SpeciesConstructor lookup performed by Promise.prototype.then.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseThenStage {
    Constructor,
    Species,
    Capability,
}

/// Observable stages of Promise.prototype.finally before it invokes `then`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseFinallyMethodStage {
    Constructor,
    Species,
    Then,
    ThenCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseCatchStage {
    Then,
    ThenCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseStaticResolveStage {
    ResolveConstructor,
    ResolveCallback,
    RejectConstructor,
    RejectCallback,
}

/// One observable boundary in an Array.prototype.forEach iteration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayForEachStage {
    Length,
    Has,
    Get,
    Callback,
}

/// The first Proxy essential internal methods routed through the exotic slow path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyInternalMethod {
    GetPrototypeOf,
    IsExtensible,
    PreventExtensions,
    PreventExtensionsObject,
}

/// One observable callback boundary in a resumable Proxy internal method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyContinuationStage {
    TrapGetter,
    TrapCall,
    ForwardResult,
}

/// Resumable callable Proxy apply/construct trap lookup and invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyCallStage {
    TrapGetter,
    TrapCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxySetPrototypeMode {
    Reflect,
    Object,
    LegacyAccessor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxySetPrototypeStage {
    TrapGetter,
    TrapCall,
    TargetIsExtensible,
    TargetGetPrototypeOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyHasStage {
    TrapGetter,
    TrapCall,
    TargetGetOwn,
    TargetIsExtensible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyGetOwnStage {
    TrapGetter,
    TrapCall,
    TargetGetOwn,
    TargetIsExtensible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyGetOwnMode {
    Descriptor,
    SetReceiver,
    HasOwn,
    Enumerable,
    LookupGetter,
    LookupSetter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ObjectLookupAccessorStage {
    GetOwn,
    GetPrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyGetStage {
    TrapGetter,
    TrapCall,
    TargetGetOwn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyDeleteMode {
    Reflect,
    Sloppy,
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyDeleteStage {
    TrapGetter,
    TrapCall,
    TargetGetOwn,
    TargetIsExtensible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyDefineMode {
    Object,
    Reflect,
    LegacyAccessor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ProxyDefineStage {
    TrapGetter,
    TrapCall,
    TargetGetOwn,
    TargetIsExtensible,
}

/// The observable operation that resumes one Map or Set iterable constructor step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum CollectionInitializerStage {
    Adder,
    IteratorMethod,
    IteratorCall,
    NextMethod,
    NextCall,
    ResultDone,
    ResultValue,
    EntryKey,
    EntryValue,
    AdderCall,
    GroupByCallback,
}

/// Observable stages of IteratorClose for a native iterable consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum CollectionIteratorCloseStage {
    ReturnGetter,
    ReturnCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum InstanceElementStage {
    Initializer,
    Define,
}

/// Observable stages after an Error message has been converted with string hint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ErrorConstructorStage {
    HasCause,
    CauseValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ErrorToStringStage {
    NameValue,
    MessageValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ObjectToLocaleStringStage {
    Get,
    Call,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeContinuationKind {
    Conversion {
        consumer: ConversionConsumer,
        stage: ToPrimitiveStage,
        callback_stage: ConversionCallbackStage,
    },
    PropertyGet(PropertyCallbackMode),
    PropertySet(PropertyWriteMode),
    ProxySet {
        mode: ProxySetMode,
        stage: ProxySetStage,
    },
    Proxy {
        operation: ProxyInternalMethod,
        stage: ProxyContinuationStage,
    },
    ProxyCall {
        construct: bool,
        stage: ProxyCallStage,
    },
    ProxySetPrototype {
        mode: ProxySetPrototypeMode,
        stage: ProxySetPrototypeStage,
    },
    ProxyHas(ProxyHasStage),
    ProxyGetOwn {
        mode: ProxyGetOwnMode,
        stage: ProxyGetOwnStage,
    },
    ProxyGet(ProxyGetStage),
    ProxyDelete {
        mode: ProxyDeleteMode,
        stage: ProxyDeleteStage,
    },
    ProxyDefine {
        mode: ProxyDefineMode,
        stage: ProxyDefineStage,
    },
    ProxyOwnKeys {
        mode: ProxyOwnKeysMode,
        stage: ProxyOwnKeysStage,
    },
    CollectionInitializer(CollectionInitializerStage),
    CollectionIteratorClose(CollectionIteratorCloseStage),
    CopyDataProperties(CopyDataPropertiesStage),
    DefineProperties(DefinePropertiesStage),
    GetOwnPropertyDescriptors(GetOwnPropertyDescriptorsStage),
    CollectionForEach,
    ArrayForEach(ArrayForEachStage),
    MapGetOrInsertComputed,
    InstanceElements(InstanceElementStage),
    InstanceOf,
    ErrorConstructor(ErrorConstructorStage),
    ErrorToString(ErrorToStringStage),
    ObjectIsPrototypeOf,
    ObjectLookupAccessor {
        stage: ObjectLookupAccessorStage,
        setter: bool,
    },
    ObjectToLocaleString(ObjectToLocaleStringStage),
    PromiseExecutor,
    PromiseReaction,
    PromiseCapabilityCall,
    PromiseThen(PromiseThenStage),
    /// Resumes a finally reaction after its user callback returns.
    PromiseFinally,
    /// Resumes finally after PromiseResolve(C, callbackResult) completes.
    PromiseFinallyResolve,
    PromiseFinallyMethod(PromiseFinallyMethodStage),
    PromiseCatch(PromiseCatchStage),
    PromiseStaticResolve(PromiseStaticResolveStage),
    PromiseResolution(PromiseResolutionMode),
    PromiseThenable,
    ConversionCallRoot,
}

/// Compact typed callback work owned by a JavaScript frame instead of the Rust call stack.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeContinuation {
    site: NativeContinuationSite,
    kind: NativeContinuationKind,
    first: Value,
    second: Value,
}

impl NativeContinuation {
    #[inline]
    pub(crate) const fn object_lookup_accessor(
        site: NativeContinuationSite,
        stage: ObjectLookupAccessorStage,
        setter: bool,
        key: Value,
        object: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ObjectLookupAccessor { stage, setter },
            first: key,
            second: object,
        }
    }

    #[inline]
    pub(crate) const fn object_is_prototype_of(
        site: NativeContinuationSite,
        prototype: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ObjectIsPrototypeOf,
            first: prototype,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn object_to_locale_string(
        site: NativeContinuationSite,
        stage: ObjectToLocaleStringStage,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ObjectToLocaleString(stage),
            first: receiver,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn error_constructor(
        site: NativeContinuationSite,
        stage: ErrorConstructorStage,
        state: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ErrorConstructor(stage),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn error_to_string(
        site: NativeContinuationSite,
        stage: ErrorToStringStage,
        state: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ErrorToString(stage),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn instance_elements(
        site: NativeContinuationSite,
        stage: InstanceElementStage,
        state: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::InstanceElements(stage),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn instance_of(site: NativeContinuationSite, prototype: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::InstanceOf,
            first: prototype,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn conversion(continuation: ConversionContinuation) -> Self {
        Self {
            site: continuation.site,
            kind: NativeContinuationKind::Conversion {
                consumer: continuation.consumer,
                stage: continuation.stage,
                callback_stage: continuation.callback_stage,
            },
            first: continuation.receiver,
            second: continuation.object,
        }
    }

    #[inline]
    pub(crate) const fn property_get(
        site: NativeContinuationSite,
        mode: PropertyCallbackMode,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertyGet(mode),
            first: receiver,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots Array iterator state and the accessor receiver while a live `[[Get]]` suspends.
    #[inline]
    pub(crate) const fn array_iterator_property_get(
        site: NativeContinuationSite,
        mode: PropertyCallbackMode,
        iterator: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertyGet(mode),
            first: iterator,
            second: receiver,
        }
    }

    #[inline]
    pub(crate) const fn property_set(
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertySet(PropertyWriteMode::Assignment),
            first: receiver,
            second: value,
        }
    }

    #[inline]
    pub(crate) const fn reflect_property_set(
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertySet(PropertyWriteMode::Reflect),
            first: receiver,
            second: value,
        }
    }

    #[inline]
    pub(crate) const fn object_assign_set(
        site: NativeContinuationSite,
        state: Value,
        value: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertySet(PropertyWriteMode::ObjectAssign),
            first: state,
            second: value,
        }
    }

    #[inline]
    pub(crate) const fn proxy_set(
        site: NativeContinuationSite,
        mode: ProxySetMode,
        stage: ProxySetStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxySet { mode, stage },
            first: state,
            second: retained,
        }
    }

    /// Roots the Proxy and handler while an accessor-backed trap lookup executes.
    #[inline]
    pub(crate) const fn proxy_trap_getter(
        site: NativeContinuationSite,
        operation: ProxyInternalMethod,
        proxy: Value,
        handler: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::Proxy {
                operation,
                stage: ProxyContinuationStage::TrapGetter,
            },
            first: proxy,
            second: handler,
        }
    }

    /// Roots a dynamically returned trap while its call executes outside the Rust stack.
    #[inline]
    pub(crate) const fn proxy_trap_call(
        site: NativeContinuationSite,
        operation: ProxyInternalMethod,
        proxy: Value,
        trap: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::Proxy {
                operation,
                stage: ProxyContinuationStage::TrapCall,
            },
            first: proxy,
            second: trap,
        }
    }

    /// Roots a Proxy and its handler while an apply/construct trap getter runs.
    #[inline]
    pub(crate) const fn proxy_call_getter(
        site: NativeContinuationSite,
        state: Value,
        handler: Value,
        construct: bool,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyCall {
                construct,
                stage: ProxyCallStage::TrapGetter,
            },
            first: state,
            second: handler,
        }
    }

    /// Roots the active Proxy and trap while the trap call executes.
    #[inline]
    pub(crate) const fn proxy_call_trap(
        site: NativeContinuationSite,
        state: Value,
        trap: Value,
        construct: bool,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyCall {
                construct,
                stage: ProxyCallStage::TrapCall,
            },
            first: state,
            second: trap,
        }
    }

    /// Preserves the outer Proxy while a nested target performs [[PreventExtensions]].
    #[inline]
    pub(crate) const fn proxy_forward_result(site: NativeContinuationSite, proxy: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::Proxy {
                operation: ProxyInternalMethod::PreventExtensionsObject,
                stage: ProxyContinuationStage::ForwardResult,
            },
            first: proxy,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn proxy_set_prototype(
        site: NativeContinuationSite,
        mode: ProxySetPrototypeMode,
        stage: ProxySetPrototypeStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxySetPrototype { mode, stage },
            first: state,
            second: retained,
        }
    }

    #[inline]
    pub(crate) const fn proxy_has(
        site: NativeContinuationSite,
        stage: ProxyHasStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyHas(stage),
            first: state,
            second: retained,
        }
    }

    #[inline]
    pub(crate) const fn proxy_get_own(
        site: NativeContinuationSite,
        mode: ProxyGetOwnMode,
        stage: ProxyGetOwnStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyGetOwn { mode, stage },
            first: state,
            second: retained,
        }
    }

    #[inline]
    pub(crate) const fn proxy_get(
        site: NativeContinuationSite,
        stage: ProxyGetStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyGet(stage),
            first: state,
            second: retained,
        }
    }

    #[inline]
    pub(crate) const fn proxy_delete(
        site: NativeContinuationSite,
        mode: ProxyDeleteMode,
        stage: ProxyDeleteStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyDelete { mode, stage },
            first: state,
            second: retained,
        }
    }

    #[inline]
    pub(crate) const fn proxy_define(
        site: NativeContinuationSite,
        mode: ProxyDefineMode,
        stage: ProxyDefineStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyDefine { mode, stage },
            first: state,
            second: retained,
        }
    }

    #[inline]
    pub(crate) const fn proxy_own_keys(
        site: NativeContinuationSite,
        mode: ProxyOwnKeysMode,
        stage: ProxyOwnKeysStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ProxyOwnKeys { mode, stage },
            first: state,
            second: retained,
        }
    }

    #[inline]
    pub(crate) const fn promise_executor(
        site: NativeContinuationSite,
        promise: Value,
        arguments: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseExecutor,
            first: promise,
            second: arguments,
        }
    }

    #[inline]
    pub(crate) const fn promise_reaction(site: NativeContinuationSite, capability: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseReaction,
            first: capability,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots a generic capability while its captured resolve or reject callback executes.
    #[inline]
    pub(crate) const fn promise_capability_call(
        site: NativeContinuationSite,
        capability: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseCapabilityCall,
            first: capability,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots staged Promise.then SpeciesConstructor state across observable Get callbacks.
    #[inline]
    pub(crate) const fn promise_then(
        site: NativeContinuationSite,
        stage: PromiseThenStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseThen(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots the original reaction argument while Promise.prototype.finally callback executes.
    #[inline]
    pub(crate) const fn promise_finally(
        site: NativeContinuationSite,
        handler: Value,
        original: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseFinally,
            first: handler,
            second: original,
        }
    }

    /// Roots finally's source/callback state across constructor, species, and `then` lookups.
    #[inline]
    pub(crate) const fn promise_finally_method(
        site: NativeContinuationSite,
        stage: PromiseFinallyMethodStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseFinallyMethod(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots catch's observable `then` lookup and call across bytecode frames.
    #[inline]
    pub(crate) const fn promise_catch(
        site: NativeContinuationSite,
        stage: PromiseCatchStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseCatch(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots finally state while a custom species constructor resolves callback output.
    #[inline]
    pub(crate) const fn promise_finally_resolve(
        site: NativeContinuationSite,
        state: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseFinallyResolve,
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots generic Promise static state across constructor and settlement callbacks.
    #[inline]
    pub(crate) const fn promise_static_resolve(
        site: NativeContinuationSite,
        stage: PromiseStaticResolveStage,
        state: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseStaticResolve(stage),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots a Promise and its resolution while observable `then` lookup executes.
    #[inline]
    pub(crate) const fn promise_resolution(
        site: NativeContinuationSite,
        mode: PromiseResolutionMode,
        promise: Value,
        resolution: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseResolution(mode),
            first: promise,
            second: resolution,
        }
    }

    /// Roots resolving functions while a thenable job calls the captured `then` method.
    #[inline]
    pub(crate) const fn promise_thenable(
        site: NativeContinuationSite,
        promise: Value,
        arguments: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseThenable,
            first: promise,
            second: arguments,
        }
    }

    /// Roots collection-constructor state while one observable protocol operation executes.
    #[inline]
    pub(crate) const fn collection_initializer(
        site: NativeContinuationSite,
        stage: CollectionInitializerStage,
        state: Value,
        callee: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::CollectionInitializer(stage),
            first: state,
            second: callee,
        }
    }

    /// Roots an iterator state and its original throw while `return` executes.
    #[inline]
    pub(crate) const fn collection_iterator_close(
        site: NativeContinuationSite,
        stage: CollectionIteratorCloseStage,
        state: Value,
        original_throw: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::CollectionIteratorClose(stage),
            first: state,
            second: original_throw,
        }
    }

    /// Roots a live collection forEach scan while its callback executes in a JS frame.
    #[inline]
    pub(crate) const fn collection_for_each(
        site: NativeContinuationSite,
        state: Value,
        callback: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::CollectionForEach,
            first: state,
            second: callback,
        }
    }

    /// Roots one fixed Array forEach state across property and callback frames.
    #[inline]
    pub(crate) const fn array_for_each(
        site: NativeContinuationSite,
        stage: ArrayForEachStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayForEach(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots pending Map upsert callback state while its JavaScript callback executes.
    #[inline]
    pub(crate) const fn map_get_or_insert_computed(
        site: NativeContinuationSite,
        state: Value,
        callback: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::MapGetOrInsertComputed,
            first: state,
            second: callback,
        }
    }

    /// Roots object-rest copy state while an enumerable source getter runs.
    #[inline]
    pub(crate) const fn copy_data_properties(site: NativeContinuationSite, state: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertyGet(PropertyCallbackMode::CopyDataProperties),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    #[inline]
    pub(crate) const fn copy_data_properties_stage(
        site: NativeContinuationSite,
        stage: CopyDataPropertiesStage,
        state: Value,
        key: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::CopyDataProperties(stage),
            first: state,
            second: key,
        }
    }

    #[inline]
    pub(crate) const fn define_properties_stage(
        site: NativeContinuationSite,
        stage: DefinePropertiesStage,
        state: Value,
        key: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::DefineProperties(stage),
            first: state,
            second: key,
        }
    }

    #[inline]
    pub(crate) const fn get_own_property_descriptors_stage(
        site: NativeContinuationSite,
        stage: GetOwnPropertyDescriptorsStage,
        state: Value,
        key: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::GetOwnPropertyDescriptors(stage),
            first: state,
            second: key,
        }
    }

    /// Roots one pending argument list while `Get(length)` or `Get(index)` executes JavaScript.
    #[inline]
    pub(crate) const fn argument_list_get(site: NativeContinuationSite, state: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertyGet(PropertyCallbackMode::ArgumentList),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots the exact call pair while the parent conversion retains its full resumable state.
    #[inline]
    pub(crate) const fn conversion_call_root(
        site: NativeContinuationSite,
        receiver: Value,
        callee: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ConversionCallRoot,
            first: receiver,
            second: callee,
        }
    }

    #[inline(always)]
    pub(crate) const fn site(self) -> NativeContinuationSite {
        self.site
    }

    #[inline]
    pub(crate) const fn kind(self) -> NativeContinuationKind {
        self.kind
    }

    #[inline]
    pub(crate) const fn first(self) -> Value {
        self.first
    }

    #[inline]
    pub(crate) const fn second(self) -> Value {
        self.second
    }

    #[inline]
    pub(crate) const fn as_conversion(self) -> Option<ConversionContinuation> {
        let NativeContinuationKind::Conversion {
            consumer,
            stage,
            callback_stage,
        } = self.kind
        else {
            return None;
        };
        Some(ConversionContinuation {
            site: self.site,
            consumer,
            receiver: self.first,
            object: self.second,
            stage,
            callback_stage,
        })
    }
}

impl Trace for NativeContinuation {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.first.trace(tracer);
        self.second.trace(tracer);
    }
}

const _: [(); 4] = [(); core::mem::size_of::<NativeContinuationKind>()];
const _: [(); 32] = [(); core::mem::size_of::<NativeContinuation>()];

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
pub(crate) struct Frame {
    pub(crate) code: CodeId,
    pub(crate) function: FunctionId,
    pub(crate) pc: WordOffset,
    pub(crate) base: u32,
    pub(crate) environment: Option<GcRef<Environment>>,
    pub(crate) return_register: Option<RegisterId>,
    pub(crate) return_continuation: bool,
    pub(crate) this_value: Value,
    pub(crate) new_target: Value,
    /// Construct receiver when `new_target` exists, otherwise a class method's `[[HomeObject]]`.
    pub(crate) receiver_or_home_object: Option<Value>,
    pub(crate) strictness: FunctionStrictness,
    pub(crate) has_finally: bool,
    pub(crate) argument_base: u32,
    pub(crate) argument_prefix: Option<GcRef<BoundFunctionData>>,
    pub(crate) argument_prefix_offset: u32,
    pub(crate) argument_prefix_count: u32,
    pub(crate) argument_count: u32,
    pub(crate) handler_base: u32,
    pub(crate) completion_base: u32,
    pub(crate) call_site: Option<WordOffset>,
}

const _: [(); 104] = [(); core::mem::size_of::<Frame>()];

/// Sparse derived-constructor state kept outside the hot `Frame` and ordinary-call path.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClassActivation {
    pub(crate) frame_depth: u32,
    pub(crate) function: Value,
}

impl Trace for ClassActivation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.function.trace(tracer);
    }
}

/// The dynamic handler state selected from immutable bytecode handler metadata.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveHandler {
    pub(crate) handler_index: u32,
    pub(crate) frame_depth: u32,
    pub(crate) environment_depth: u32,
}

/// Sparse direct-eval var record owned by exactly one JavaScript activation depth.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EvalVarEnvironment {
    pub(crate) frame_depth: u32,
    pub(crate) environment: GcRef<Environment>,
}

impl Trace for EvalVarEnvironment {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.environment.trace(tracer);
    }
}

#[derive(Debug, Default)]
pub(crate) struct Fiber {
    pub(crate) frames: Vec<Frame>,
    /// Only direct-eval/debugger fibers enable runtime name lookup before global resolution.
    pub(crate) dynamic_scope: bool,
    /// Distinguishes an eval child from an ordinary fiber that only retains var overlays.
    pub(crate) direct_eval: bool,
    /// Activation-aligned lazy Arguments object roots; avoids growing the hot Frame layout.
    pub(crate) argument_objects: Vec<Option<Value>>,
    /// Activation-aligned roots for native-owned argument suffixes; keeps the hot Frame at 104B.
    pub(crate) argument_sources: Vec<Option<GcRef<NativeCallState>>>,
    /// Activation-aligned callable roots for lazy sloppy `arguments.callee` publication.
    pub(crate) argument_callees: Vec<Option<Value>>,
    /// Only derived constructors enter this stack, preserving the ordinary call hot path.
    pub(crate) derived_activations: Vec<ClassActivation>,
    /// Only base class constructors enter this stack; ordinary functions never pay its cost.
    pub(crate) base_class_activations: Vec<ClassActivation>,
    /// Frame depths for active class-name lexical environments, sparse across ordinary execution.
    pub(crate) class_environments: Vec<u32>,
    /// Persistent sloppy-eval var records, sparse across direct-eval-capable activations.
    pub(crate) eval_var_environments: Vec<EvalVarEnvironment>,
    pub(crate) registers: Vec<Value>,
    pub(crate) handlers: Vec<ActiveHandler>,
    pub(crate) completions: CompletionStack,
    pub(crate) pending_exception: Option<Value>,
}

impl Fiber {
    /// Traces every mutable reference reachable from an active, yielded, or suspended fiber.
    ///
    /// Frame control indices are validated when handlers are installed. They do not themselves
    /// own heap references, while registers, frame context, and abrupt completion payloads do.
    pub(crate) fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.registers.trace(tracer);
        self.argument_objects.trace(tracer);
        self.argument_sources.trace(tracer);
        self.argument_callees.trace(tracer);
        self.derived_activations.trace(tracer);
        self.base_class_activations.trace(tracer);
        self.eval_var_environments.trace(tracer);
        debug_assert_eq!(self.argument_objects.len(), self.frames.len());
        debug_assert_eq!(self.argument_sources.len(), self.frames.len());
        debug_assert_eq!(self.argument_callees.len(), self.frames.len());
        debug_assert!(self.derived_activations.iter().all(|activation| {
            activation.frame_depth != 0 && activation.frame_depth as usize <= self.frames.len()
        }));
        debug_assert!(self.base_class_activations.iter().all(|activation| {
            activation.frame_depth != 0 && activation.frame_depth as usize <= self.frames.len()
        }));
        debug_assert!(self.class_environments.iter().all(|depth| {
            *depth != 0 && usize::try_from(*depth).is_ok_and(|depth| depth <= self.frames.len())
        }));
        debug_assert!(self.eval_var_environments.iter().all(|environment| {
            environment.frame_depth != 0 && environment.frame_depth as usize <= self.frames.len()
        }));
        for frame in &mut self.frames {
            frame.environment.trace(tracer);
            frame.this_value.trace(tracer);
            frame.new_target.trace(tracer);
            frame.receiver_or_home_object.trace(tracer);
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
        self.completions.trace(tracer);
        self.pending_exception.trace(tracer);
    }
}
