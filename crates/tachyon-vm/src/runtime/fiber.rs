//! Explicit fiber, frame, continuation, and GC root state.

use super::super::*;
use super::completion::CompletionStack;

pub(crate) struct VmRoots<'a> {
    pub(crate) fiber: &'a mut Fiber,
    pub(crate) finalization_jobs: &'a mut finalization::FinalizationJobs,
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
    HasOwn,
    ReflectDeleteProperty,
    ReflectHas,
    ReflectGet,
    ReflectSet,
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
    GlobalIsFinite,
    GlobalIsNaN,
    GlobalParseFloat,
    GlobalParseInt,
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
            NativeFunction::GlobalIsFinite => Some(Self::GlobalIsFinite),
            NativeFunction::GlobalIsNaN => Some(Self::GlobalIsNaN),
            NativeFunction::GlobalParseFloat => Some(Self::GlobalParseFloat),
            NativeFunction::GlobalParseInt => Some(Self::GlobalParseInt),
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
            Self::GlobalIsFinite => NativeFunction::GlobalIsFinite,
            Self::GlobalIsNaN => NativeFunction::GlobalIsNaN,
            Self::GlobalParseFloat => NativeFunction::GlobalParseFloat,
            Self::GlobalParseInt => NativeFunction::GlobalParseInt,
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
            | Self::BuiltinPropertyKey(_) => None,
        }
    }

    #[inline]
    pub(crate) const fn uses_string_hint(self) -> bool {
        matches!(
            self,
            Self::NativeCall(ConversionNativeFunction::StringConstructor)
                | Self::NativeCall(ConversionNativeFunction::GlobalParseFloat)
                | Self::NativeCall(ConversionNativeFunction::GlobalParseInt)
                | Self::ToPropertyKey
                | Self::BuiltinPropertyKey(_)
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
    CopyDataProperties,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PropertyWriteMode {
    Assignment,
    Reflect,
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
    CollectionInitializer(CollectionInitializerStage),
    CollectionForEach,
    MapGetOrInsertComputed,
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
    pub(crate) construct_receiver: Option<Value>,
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

/// The dynamic handler state selected from immutable bytecode handler metadata.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveHandler {
    pub(crate) handler_index: u32,
    pub(crate) frame_depth: u32,
    pub(crate) environment_depth: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Fiber {
    pub(crate) frames: Vec<Frame>,
    /// Activation-aligned lazy Arguments object roots; avoids growing the hot Frame layout.
    pub(crate) argument_objects: Vec<Option<Value>>,
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
        debug_assert_eq!(self.argument_objects.len(), self.frames.len());
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
        self.completions.trace(tracer);
        self.pending_exception.trace(tracer);
    }
}
