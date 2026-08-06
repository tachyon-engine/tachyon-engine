//! Explicit fiber, frame, continuation, and GC root state.

use super::super::*;
use super::completion::CompletionStack;
use crate::module::ModuleGraph;

pub(crate) struct VmRoots<'a> {
    pub(crate) fiber: &'a mut Fiber,
    pub(crate) suspended_fibers: &'a mut Vec<Fiber>,
    pub(crate) finalization_jobs: &'a mut finalization::FinalizationJobs,
    pub(crate) promise_jobs: &'a mut PromiseJobQueue,
    pub(crate) realm: &'a mut Realm,
    pub(crate) inactive_realms: &'a mut Vec<(RealmId, Realm)>,
    pub(crate) loaded_code: &'a mut Vec<LoadedCode>,
    pub(crate) module_graph: &'a mut ModuleGraph,
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

pub(crate) struct ConstructReceiverRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) site: CallSite,
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

impl Trace for ConstructReceiverRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.site.trace(tracer);
    }
}

impl Trace for VmRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        for fiber in self.suspended_fibers.iter_mut() {
            fiber.trace_roots(tracer);
        }
        self.finalization_jobs.trace(tracer);
        self.promise_jobs.trace(tracer);
        self.realm.trace(tracer);
        for (_, realm) in self.inactive_realms.iter_mut() {
            realm.trace(tracer);
        }
        for code in self.loaded_code.iter_mut() {
            code.trace(tracer);
        }
        self.module_graph.trace(tracer);
    }
}

pub(crate) struct CodeLoadRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) constant_values: &'a mut Vec<Option<Value>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

/// Closed identity for generic, non-RegExp String prototype algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StringPrototypeOperation {
    CharAt,
    CharCodeAt,
    At,
    CodePointAt,
    Slice,
    Substring,
    IndexOf,
    LastIndexOf,
    Repeat,
    PadStart,
    PadEnd,
    IsWellFormed,
    ToWellFormed,
    Includes,
    StartsWith,
    EndsWith,
}

impl StringPrototypeOperation {
    pub(crate) const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::CharAt,
            1 => Self::CharCodeAt,
            2 => Self::At,
            3 => Self::CodePointAt,
            4 => Self::Slice,
            5 => Self::Substring,
            6 => Self::IndexOf,
            7 => Self::LastIndexOf,
            8 => Self::Repeat,
            9 => Self::PadStart,
            10 => Self::PadEnd,
            11 => Self::IsWellFormed,
            12 => Self::ToWellFormed,
            13 => Self::Includes,
            14 => Self::StartsWith,
            15 => Self::EndsWith,
            _ => return None,
        })
    }
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
    SymbolConstructor,
    SymbolFor,
    NumberConstructor,
    BigIntConstructor,
    BigIntAsIntN,
    BigIntAsUintN,
    BigIntToString,
    NumberToExponential,
    NumberToFixed,
    NumberToPrecision,
    NumberToString,
    StringToLowerCase,
    StringToUpperCase,
    StringToLocaleLowerCase,
    StringToLocaleUpperCase,
    StringTrim,
    StringTrimStart,
    StringTrimEnd,
    StringIterator,
    GlobalIsFinite,
    GlobalIsNaN,
    GlobalParseFloat,
    GlobalParseInt,
    GlobalDecodeUri,
    GlobalDecodeUriComponent,
    GlobalEncodeUri,
    GlobalEncodeUriComponent,
    DateParse,
}

impl ConversionNativeFunction {
    pub(crate) const fn from_native(native: NativeFunction) -> Option<Self> {
        match native {
            NativeFunction::StringConstructor => Some(Self::StringConstructor),
            NativeFunction::SymbolConstructor => Some(Self::SymbolConstructor),
            NativeFunction::SymbolFor => Some(Self::SymbolFor),
            NativeFunction::NumberConstructor => Some(Self::NumberConstructor),
            NativeFunction::BigIntConstructor => Some(Self::BigIntConstructor),
            NativeFunction::BigIntAsIntN => Some(Self::BigIntAsIntN),
            NativeFunction::BigIntAsUintN => Some(Self::BigIntAsUintN),
            NativeFunction::BigIntToString => Some(Self::BigIntToString),
            NativeFunction::NumberToExponential => Some(Self::NumberToExponential),
            NativeFunction::NumberToFixed => Some(Self::NumberToFixed),
            NativeFunction::NumberToPrecision => Some(Self::NumberToPrecision),
            NativeFunction::NumberToString => Some(Self::NumberToString),
            NativeFunction::StringToLowerCase => Some(Self::StringToLowerCase),
            NativeFunction::StringToUpperCase => Some(Self::StringToUpperCase),
            NativeFunction::StringToLocaleLowerCase => Some(Self::StringToLocaleLowerCase),
            NativeFunction::StringToLocaleUpperCase => Some(Self::StringToLocaleUpperCase),
            NativeFunction::StringTrim => Some(Self::StringTrim),
            NativeFunction::StringTrimStart => Some(Self::StringTrimStart),
            NativeFunction::StringTrimEnd => Some(Self::StringTrimEnd),
            NativeFunction::StringIterator => Some(Self::StringIterator),
            NativeFunction::GlobalIsFinite => Some(Self::GlobalIsFinite),
            NativeFunction::GlobalIsNaN => Some(Self::GlobalIsNaN),
            NativeFunction::GlobalParseFloat => Some(Self::GlobalParseFloat),
            NativeFunction::GlobalParseInt => Some(Self::GlobalParseInt),
            NativeFunction::GlobalDecodeUri => Some(Self::GlobalDecodeUri),
            NativeFunction::GlobalDecodeUriComponent => Some(Self::GlobalDecodeUriComponent),
            NativeFunction::GlobalEncodeUri => Some(Self::GlobalEncodeUri),
            NativeFunction::GlobalEncodeUriComponent => Some(Self::GlobalEncodeUriComponent),
            NativeFunction::DateParse => Some(Self::DateParse),
            _ => None,
        }
    }

    pub(crate) const fn native(self) -> NativeFunction {
        match self {
            Self::StringConstructor => NativeFunction::StringConstructor,
            Self::SymbolConstructor => NativeFunction::SymbolConstructor,
            Self::SymbolFor => NativeFunction::SymbolFor,
            Self::NumberConstructor => NativeFunction::NumberConstructor,
            Self::BigIntConstructor => NativeFunction::BigIntConstructor,
            Self::BigIntAsIntN => NativeFunction::BigIntAsIntN,
            Self::BigIntAsUintN => NativeFunction::BigIntAsUintN,
            Self::BigIntToString => NativeFunction::BigIntToString,
            Self::NumberToExponential => NativeFunction::NumberToExponential,
            Self::NumberToFixed => NativeFunction::NumberToFixed,
            Self::NumberToPrecision => NativeFunction::NumberToPrecision,
            Self::NumberToString => NativeFunction::NumberToString,
            Self::StringToLowerCase => NativeFunction::StringToLowerCase,
            Self::StringToUpperCase => NativeFunction::StringToUpperCase,
            Self::StringToLocaleLowerCase => NativeFunction::StringToLocaleLowerCase,
            Self::StringToLocaleUpperCase => NativeFunction::StringToLocaleUpperCase,
            Self::StringTrim => NativeFunction::StringTrim,
            Self::StringTrimStart => NativeFunction::StringTrimStart,
            Self::StringTrimEnd => NativeFunction::StringTrimEnd,
            Self::StringIterator => NativeFunction::StringIterator,
            Self::GlobalIsFinite => NativeFunction::GlobalIsFinite,
            Self::GlobalIsNaN => NativeFunction::GlobalIsNaN,
            Self::GlobalParseFloat => NativeFunction::GlobalParseFloat,
            Self::GlobalParseInt => NativeFunction::GlobalParseInt,
            Self::GlobalDecodeUri => NativeFunction::GlobalDecodeUri,
            Self::GlobalDecodeUriComponent => NativeFunction::GlobalDecodeUriComponent,
            Self::GlobalEncodeUri => NativeFunction::GlobalEncodeUri,
            Self::GlobalEncodeUriComponent => NativeFunction::GlobalEncodeUriComponent,
            Self::DateParse => NativeFunction::DateParse,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConversionConsumer {
    NativeCall(ConversionNativeFunction),
    NativeConstruct(ConversionNativeFunction),
    ToNumber,
    ToString,
    DynamicImportSource,
    StringConcatElement,
    StringFromCodesElement,
    MathArgument,
    StringRawLength,
    StringRawLiteral,
    StringRawSubstitution,
    Negate,
    Update(bool),
    BitwiseNot,
    BinaryLeft(Opcode),
    BinaryRight(Opcode),
    AddLeft,
    AddRight,
    RelationalLeft(Opcode),
    RelationalRight(Opcode),
    Equality(Opcode),
    BigIntAsNValue(bool),
    ToPropertyKey,
    BuiltinPropertyKey(BuiltinPropertyKeyConsumer),
    ArraySetLengthUint32,
    ArraySetLengthNumber,
    SetRecordSize,
    ErrorConstructorMessage,
    ErrorToStringName,
    ErrorToStringMessage,
    DynamicFunctionArgument,
    DateConstructSingle,
    DateNumericArgument,
    DateToPrimitiveString,
    DateToPrimitiveNumber,
    DateToJson,
    IntlLocaleListLength,
    IntlLocaleListElement,
    IntlSupportedValuesKey,
    IntlCollatorOption,
    IntlCollatorCompareLeft,
    IntlCollatorCompareRight,
    JsonParseText,
    JsonStringifyNumberSpace,
    JsonStringifyStringSpace,
    JsonStringifyNumberValue,
    JsonStringifyStringValue,
    JsonStringifyArrayLength,
    JsonStringifyPropertyListLength,
    JsonStringifyPropertyListString,
    RegExpExecInput,
    RegExpTestInput,
    RegExpSearchInput,
    RegExpReplaceResult,
    RegExpStringIteratorMatch,
    RegExpStringIteratorLastIndex,
    StringSearchReceiver,
    StringSearchPattern,
    StringMatchReceiver,
    StringMatchPattern,
    StringMatchAllFlags,
    RegExpLastIndex,
    ArrayLength,
    ArraySearchIndex,
    ArrayJoinLength,
    ArrayJoinSeparator,
    ArrayJoinElement,
    ArrayConcatLength,
    ArrayFlatLength,
    ArrayFlatDepth,
    ArrayFlatElementLength,
    ArrayFlatMapLength,
    ArrayFlatMapInnerLength,
    ArrayCopyLength,
    ArrayCopyIndex,
    ArrayCopyStart,
    ArrayCopyDeleteCount,
    ArrayCopyWithinLength,
    ArrayCopyWithinTarget,
    ArrayCopyWithinStart,
    ArrayCopyWithinEnd,
    ArrayToSortedLength,
    ArrayToSortedCompareResult,
    ArrayToSortedLeftString,
    ArrayToSortedRightString,
    ArrayStaticLength,
    ArraySliceLength,
    ArraySliceStart,
    ArraySliceEnd,
    ArrayBufferSliceStart,
    ArrayBufferSliceEnd,
    ArrayBufferTransferLength(bool),
    SharedArrayBufferLength,
    SharedArrayBufferMaxByteLength,
    SharedArrayBufferGrowLength,
    SharedArrayBufferSliceStart,
    SharedArrayBufferSliceEnd,
    AtomicsIndex(AtomicsFunction),
    AtomicsValue(AtomicsFunction),
    AtomicsReplacement(AtomicsFunction),
    AtomicsIsLockFree,
    AtomicsNotifyIndex,
    AtomicsNotifyCount,
    AtomicsWaitIndex(AtomicsFunction),
    AtomicsWaitExpected(AtomicsFunction),
    AtomicsWaitTimeout(AtomicsFunction),
    ArraySpliceLength,
    ArraySpliceStart,
    ArraySpliceDeleteCount,
    ArrayRemoveLength,
    ArrayInsertLength,
    ArrayReverseLength,
    ArrayFillLength,
    ArrayFillStart,
    ArrayFillEnd,
    StringSplitReceiver,
    StringSplitLimit,
    StringSplitSeparator,
    StringReplaceAllFlags,
    StringReplaceAllReceiver,
    StringReplaceAllSearch,
    StringReplaceAllReplacement,
    StringPrototypeReceiver,
    StringPrototypeString,
    StringPrototypeFiller,
    StringPrototypeFirstNumber,
    StringPrototypeSecondNumber,
    StringUnicodeReceiver,
    StringUnicodeArgument,
    TypedArrayByteOffset,
    TypedArrayLength,
    TypedArrayElement,
    TypedArrayIndexSet,
    TypedArrayStaticElement,
    TypedArrayAtIndex,
    TypedArrayWithIndex,
    TypedArrayWithValue,
    TypedArrayIncludesFromIndex,
    TypedArrayFillValue,
    TypedArrayFillStart,
    TypedArrayFillEnd,
    TypedArrayCopyWithinTarget,
    TypedArrayCopyWithinStart,
    TypedArrayCopyWithinEnd,
    TypedArraySetOffset,
    TypedArraySetLength,
    TypedArraySetElement,
    TypedArrayJoinSeparator,
    TypedArraySliceStart,
    TypedArraySliceEnd,
    TypedArraySubarrayStart,
    TypedArraySubarrayEnd,
    TypedArrayTransformElement,
    TypedArraySearchFromIndex,
}

impl ConversionConsumer {
    #[inline]
    pub(crate) const fn native(self) -> Option<NativeFunction> {
        match self {
            Self::NativeCall(native) | Self::NativeConstruct(native) => Some(native.native()),
            Self::ToNumber
            | Self::ToString
            | Self::DynamicImportSource
            | Self::StringConcatElement
            | Self::StringFromCodesElement
            | Self::MathArgument
            | Self::StringRawLength
            | Self::StringRawLiteral
            | Self::StringRawSubstitution
            | Self::Negate
            | Self::Update(_)
            | Self::BitwiseNot
            | Self::BinaryLeft(_)
            | Self::BinaryRight(_)
            | Self::AddLeft
            | Self::AddRight
            | Self::RelationalLeft(_)
            | Self::RelationalRight(_)
            | Self::Equality(_)
            | Self::BigIntAsNValue(_)
            | Self::ToPropertyKey
            | Self::BuiltinPropertyKey(_)
            | Self::ArraySetLengthUint32
            | Self::ArraySetLengthNumber
            | Self::SetRecordSize
            | Self::ErrorConstructorMessage
            | Self::ErrorToStringName
            | Self::ErrorToStringMessage
            | Self::DynamicFunctionArgument
            | Self::DateConstructSingle
            | Self::DateNumericArgument
            | Self::DateToPrimitiveString
            | Self::DateToPrimitiveNumber
            | Self::DateToJson
            | Self::IntlLocaleListLength
            | Self::IntlLocaleListElement
            | Self::IntlSupportedValuesKey
            | Self::IntlCollatorOption
            | Self::IntlCollatorCompareLeft
            | Self::IntlCollatorCompareRight
            | Self::JsonParseText
            | Self::JsonStringifyNumberSpace
            | Self::JsonStringifyStringSpace
            | Self::JsonStringifyNumberValue
            | Self::JsonStringifyStringValue
            | Self::JsonStringifyArrayLength
            | Self::JsonStringifyPropertyListLength
            | Self::JsonStringifyPropertyListString
            | Self::RegExpExecInput
            | Self::RegExpTestInput
            | Self::RegExpSearchInput
            | Self::RegExpReplaceResult
            | Self::RegExpStringIteratorMatch
            | Self::RegExpStringIteratorLastIndex
            | Self::StringSearchReceiver
            | Self::StringSearchPattern
            | Self::StringMatchReceiver
            | Self::StringMatchPattern
            | Self::StringMatchAllFlags
            | Self::RegExpLastIndex
            | Self::ArrayLength
            | Self::ArraySearchIndex
            | Self::ArrayJoinLength
            | Self::ArrayJoinSeparator
            | Self::ArrayJoinElement
            | Self::ArrayConcatLength
            | Self::ArrayFlatLength
            | Self::ArrayFlatDepth
            | Self::ArrayFlatElementLength
            | Self::ArrayFlatMapLength
            | Self::ArrayFlatMapInnerLength
            | Self::ArrayCopyLength
            | Self::ArrayCopyIndex
            | Self::ArrayCopyStart
            | Self::ArrayCopyDeleteCount
            | Self::ArrayCopyWithinLength
            | Self::ArrayCopyWithinTarget
            | Self::ArrayCopyWithinStart
            | Self::ArrayCopyWithinEnd
            | Self::ArrayToSortedLength
            | Self::ArrayToSortedCompareResult
            | Self::ArrayToSortedLeftString
            | Self::ArrayToSortedRightString
            | Self::ArrayStaticLength
            | Self::ArraySliceLength
            | Self::ArraySliceStart
            | Self::ArraySliceEnd
            | Self::ArrayBufferSliceStart
            | Self::ArrayBufferSliceEnd
            | Self::ArrayBufferTransferLength(_)
            | Self::SharedArrayBufferLength
            | Self::SharedArrayBufferMaxByteLength
            | Self::SharedArrayBufferGrowLength
            | Self::SharedArrayBufferSliceStart
            | Self::SharedArrayBufferSliceEnd
            | Self::AtomicsIndex(_)
            | Self::AtomicsValue(_)
            | Self::AtomicsReplacement(_)
            | Self::AtomicsIsLockFree
            | Self::AtomicsNotifyIndex
            | Self::AtomicsNotifyCount
            | Self::AtomicsWaitIndex(_)
            | Self::AtomicsWaitExpected(_)
            | Self::AtomicsWaitTimeout(_)
            | Self::ArraySpliceLength
            | Self::ArraySpliceStart
            | Self::ArraySpliceDeleteCount
            | Self::ArrayRemoveLength
            | Self::ArrayInsertLength
            | Self::ArrayReverseLength => None,
            Self::ArrayFillLength
            | Self::ArrayFillStart
            | Self::ArrayFillEnd
            | Self::StringSplitReceiver
            | Self::StringSplitLimit
            | Self::StringSplitSeparator
            | Self::StringReplaceAllFlags
            | Self::StringReplaceAllReceiver
            | Self::StringReplaceAllSearch
            | Self::StringReplaceAllReplacement
            | Self::StringPrototypeReceiver
            | Self::StringPrototypeString
            | Self::StringPrototypeFiller
            | Self::StringPrototypeFirstNumber
            | Self::StringPrototypeSecondNumber
            | Self::StringUnicodeReceiver
            | Self::StringUnicodeArgument
            | Self::TypedArrayByteOffset
            | Self::TypedArrayLength
            | Self::TypedArrayElement
            | Self::TypedArrayIndexSet
            | Self::TypedArrayStaticElement
            | Self::TypedArrayAtIndex
            | Self::TypedArrayWithIndex
            | Self::TypedArrayWithValue
            | Self::TypedArrayTransformElement => None,
            Self::TypedArrayIncludesFromIndex
            | Self::TypedArrayFillValue
            | Self::TypedArrayFillStart
            | Self::TypedArrayFillEnd
            | Self::TypedArrayCopyWithinTarget
            | Self::TypedArrayCopyWithinStart
            | Self::TypedArrayCopyWithinEnd
            | Self::TypedArraySetOffset
            | Self::TypedArraySetLength
            | Self::TypedArraySetElement
            | Self::TypedArrayJoinSeparator
            | Self::TypedArraySliceStart
            | Self::TypedArraySliceEnd
            | Self::TypedArraySubarrayStart
            | Self::TypedArraySubarrayEnd
            | Self::TypedArraySearchFromIndex => None,
        }
    }

    #[inline]
    pub(crate) const fn uses_string_hint(self) -> bool {
        matches!(
            self,
            Self::NativeCall(ConversionNativeFunction::StringConstructor)
                | Self::NativeCall(ConversionNativeFunction::SymbolConstructor)
                | Self::NativeCall(ConversionNativeFunction::SymbolFor)
                | Self::NativeCall(ConversionNativeFunction::StringToLowerCase)
                | Self::NativeCall(ConversionNativeFunction::StringToUpperCase)
                | Self::NativeCall(ConversionNativeFunction::StringToLocaleLowerCase)
                | Self::NativeCall(ConversionNativeFunction::StringToLocaleUpperCase)
                | Self::NativeCall(ConversionNativeFunction::StringTrim)
                | Self::NativeCall(ConversionNativeFunction::StringTrimStart)
                | Self::NativeCall(ConversionNativeFunction::StringTrimEnd)
                | Self::NativeCall(ConversionNativeFunction::StringIterator)
                | Self::NativeCall(ConversionNativeFunction::GlobalParseFloat)
                | Self::NativeCall(ConversionNativeFunction::GlobalParseInt)
                | Self::NativeCall(ConversionNativeFunction::GlobalDecodeUri)
                | Self::NativeCall(ConversionNativeFunction::GlobalDecodeUriComponent)
                | Self::NativeCall(ConversionNativeFunction::GlobalEncodeUri)
                | Self::NativeCall(ConversionNativeFunction::GlobalEncodeUriComponent)
                | Self::NativeCall(ConversionNativeFunction::DateParse)
                | Self::ToString
                | Self::DynamicImportSource
                | Self::StringConcatElement
                | Self::StringRawLiteral
                | Self::StringRawSubstitution
                | Self::ToPropertyKey
                | Self::BuiltinPropertyKey(_)
                | Self::ErrorConstructorMessage
                | Self::ErrorToStringName
                | Self::ErrorToStringMessage
                | Self::DynamicFunctionArgument
                | Self::DateToPrimitiveString
                | Self::IntlLocaleListElement
                | Self::IntlSupportedValuesKey
                | Self::IntlCollatorOption
                | Self::IntlCollatorCompareLeft
                | Self::IntlCollatorCompareRight
                | Self::JsonParseText
                | Self::JsonStringifyStringSpace
                | Self::JsonStringifyStringValue
                | Self::JsonStringifyPropertyListString
                | Self::RegExpExecInput
                | Self::RegExpTestInput
                | Self::RegExpSearchInput
                | Self::RegExpReplaceResult
                | Self::RegExpStringIteratorMatch
                | Self::RegExpStringIteratorLastIndex
                | Self::StringSearchReceiver
                | Self::StringSearchPattern
                | Self::StringMatchReceiver
                | Self::StringMatchPattern
                | Self::StringMatchAllFlags
                | Self::ArrayToSortedLeftString
                | Self::ArrayToSortedRightString
                | Self::ArrayJoinSeparator
                | Self::ArrayJoinElement
                | Self::TypedArrayJoinSeparator
                | Self::StringSplitReceiver
                | Self::StringSplitSeparator
                | Self::StringReplaceAllFlags
                | Self::StringReplaceAllReceiver
                | Self::StringReplaceAllSearch
                | Self::StringReplaceAllReplacement
                | Self::StringPrototypeReceiver
                | Self::StringPrototypeString
                | Self::StringPrototypeFiller
                | Self::StringUnicodeReceiver
                | Self::StringUnicodeArgument
        )
    }

    #[inline]
    pub(crate) const fn preferred_type(self) -> PreferredType {
        if self.uses_string_hint() {
            PreferredType::String
        } else if matches!(
            self,
            Self::AddLeft | Self::AddRight | Self::Equality(_) | Self::DateConstructSingle
        ) {
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
                | Self::ToString
                | Self::DynamicImportSource
                | Self::StringConcatElement
                | Self::StringFromCodesElement
                | Self::MathArgument
                | Self::StringRawLength
                | Self::StringRawLiteral
                | Self::StringRawSubstitution
                | Self::Negate
                | Self::Update(_)
                | Self::BitwiseNot
                | Self::BinaryLeft(_)
                | Self::BinaryRight(_)
                | Self::AddLeft
                | Self::AddRight
                | Self::RelationalLeft(_)
                | Self::RelationalRight(_)
                | Self::Equality(_)
                | Self::BigIntAsNValue(_)
                | Self::ToPropertyKey
                | Self::BuiltinPropertyKey(_)
                | Self::ErrorConstructorMessage
                | Self::ErrorToStringName
                | Self::ErrorToStringMessage
                | Self::DynamicFunctionArgument
                | Self::DateConstructSingle
                | Self::DateNumericArgument
                | Self::DateToPrimitiveString
                | Self::DateToPrimitiveNumber
                | Self::DateToJson
                | Self::IntlLocaleListLength
                | Self::IntlLocaleListElement
                | Self::IntlSupportedValuesKey
                | Self::IntlCollatorOption
                | Self::IntlCollatorCompareLeft
                | Self::IntlCollatorCompareRight
                | Self::JsonParseText
                | Self::JsonStringifyNumberSpace
                | Self::JsonStringifyStringSpace
                | Self::JsonStringifyNumberValue
                | Self::JsonStringifyStringValue
                | Self::JsonStringifyArrayLength
                | Self::JsonStringifyPropertyListLength
                | Self::JsonStringifyPropertyListString
                | Self::RegExpExecInput
                | Self::RegExpTestInput
                | Self::RegExpSearchInput
                | Self::RegExpReplaceResult
                | Self::RegExpStringIteratorMatch
                | Self::RegExpStringIteratorLastIndex
                | Self::StringSearchReceiver
                | Self::StringSearchPattern
                | Self::StringMatchReceiver
                | Self::StringMatchPattern
                | Self::StringMatchAllFlags
                | Self::RegExpLastIndex
                | Self::ArrayLength
                | Self::ArrayJoinLength
                | Self::ArrayJoinSeparator
                | Self::ArrayJoinElement
                | Self::ArrayConcatLength
                | Self::ArrayFlatLength
                | Self::ArrayFlatDepth
                | Self::ArrayFlatElementLength
                | Self::ArrayFlatMapLength
                | Self::ArrayFlatMapInnerLength
                | Self::ArraySliceLength
                | Self::ArraySliceStart
                | Self::ArraySliceEnd
                | Self::ArraySpliceLength
                | Self::ArraySpliceStart
                | Self::ArraySpliceDeleteCount
                | Self::ArrayRemoveLength
                | Self::ArrayInsertLength
                | Self::ArrayReverseLength
                | Self::ArrayFillLength
                | Self::ArrayFillStart
                | Self::ArrayFillEnd
                | Self::StringSplitReceiver
                | Self::StringSplitLimit
                | Self::StringSplitSeparator
                | Self::StringReplaceAllFlags
                | Self::StringReplaceAllReceiver
                | Self::StringReplaceAllSearch
                | Self::StringReplaceAllReplacement
                | Self::StringPrototypeReceiver
                | Self::StringPrototypeString
                | Self::StringPrototypeFiller
                | Self::StringPrototypeFirstNumber
                | Self::StringPrototypeSecondNumber
                | Self::StringUnicodeReceiver
                | Self::StringUnicodeArgument
                | Self::TypedArrayByteOffset
                | Self::TypedArrayLength
                | Self::TypedArrayElement
                | Self::TypedArrayIndexSet
                | Self::TypedArrayStaticElement
                | Self::TypedArrayAtIndex
                | Self::TypedArrayWithIndex
                | Self::TypedArrayWithValue
                | Self::TypedArrayIncludesFromIndex
                | Self::TypedArraySearchFromIndex
                | Self::TypedArrayJoinSeparator
                | Self::TypedArraySliceStart
                | Self::TypedArraySliceEnd
                | Self::TypedArraySubarrayStart
                | Self::TypedArraySubarrayEnd
                | Self::AtomicsIndex(_)
                | Self::AtomicsValue(_)
                | Self::AtomicsReplacement(_)
                | Self::AtomicsWaitIndex(_)
                | Self::AtomicsWaitExpected(_)
                | Self::AtomicsWaitTimeout(_)
                | Self::ArrayCopyWithinLength
                | Self::ArrayCopyWithinTarget
                | Self::ArrayCopyWithinStart
                | Self::ArrayCopyWithinEnd
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
    IntlCollator,
    CopyDataProperties,
    DefineProperties,
}

/// Resumable stages shared by `instanceof` and the default `@@hasInstance` builtin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum InstanceOfStage {
    MethodGet,
    MethodCall,
    PrototypeGet,
    PrototypeChain,
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
    Internal,
    AsyncAwait,
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
    ConstructorPrototype,
    ResolveConstructor,
    ResolveCallback,
    RejectConstructor,
    RejectCallback,
    TryConstructor,
    TryCallback,
    TryResolve,
    TryReject,
}

/// One observable boundary in an Array.prototype.forEach iteration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayForEachStage {
    Length,
    OutputConstructor,
    OutputSpecies,
    OutputConstruct,
    OutputDefine,
    Has,
    Get,
    Callback,
    ReduceHas,
    ReduceGet,
    ReduceCallback,
    SearchHas,
    SearchGet,
    FindGet,
    FindCallback,
}

/// Observable callback boundary in fixed TypedArray predicate and search methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypedArrayCallbackStage {
    Callback,
}

/// One observable boundary in the resumable Array.prototype.concat algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayConcatStage {
    SpeciesConstructor,
    SpeciesValue,
    SpeciesConstruct,
    Spreadable,
    Length,
    ElementHas,
    ElementGet,
    ElementDefine,
    ValueDefine,
    FinalLength,
}

/// One observable boundary in the resumable Array.prototype.flatMap algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayFlatMapStage {
    Length,
    SpeciesConstructor,
    SpeciesValue,
    SpeciesConstruct,
    SourceHas,
    SourceGet,
    Callback,
    InnerLength,
    InnerHas,
    InnerGet,
    Define,
}

/// One observable boundary in the resumable Array.prototype.flat algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayFlatStage {
    Length,
    SpeciesConstructor,
    SpeciesValue,
    SpeciesConstruct,
    SourceHas,
    SourceGet,
    ElementLength,
    Define,
}

/// One observable boundary in the resumable Array.prototype.slice algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArraySliceStage {
    Length,
    SpeciesConstructor,
    SpeciesValue,
    SpeciesConstruct,
    ElementHas,
    ElementGet,
    ElementDefine,
    FinalLength,
}

/// One observable constructor/species boundary in fixed ArrayBuffer slicing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayBufferSliceStage {
    Constructor,
    Value,
    Construct,
    SharedConstructor,
    SharedValue,
    SharedConstruct,
}

/// Observable property reads in SharedArrayBuffer construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum SharedArrayBufferConstructorStage {
    Maximum,
    Prototype,
}

/// One observable boundary in the resumable Array.prototype.splice algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArraySpliceStage {
    Length,
    SpeciesConstructor,
    SpeciesValue,
    SpeciesConstruct,
    CopyHas,
    CopyGet,
    CopyDefine,
    ResultLength,
    MoveHas,
    MoveGet,
    MoveSet,
    MoveDelete,
    TailDelete,
    InsertSet,
    FinalLength,
}

/// One observable boundary in the resumable Array pop/shift algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayRemoveStage {
    Length,
    ElementGet,
    SourceHas,
    SourceGet,
    TargetSet,
    TargetDelete,
    TailDelete,
    FinalLength,
}

/// One observable boundary in the resumable Array push/unshift algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayInsertStage {
    Length,
    MoveHas,
    MoveGet,
    MoveSet,
    MoveDelete,
    ItemSet,
    FinalLength,
}

/// One observable boundary in the resumable Array reverse algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayReverseStage {
    Length,
    LowerHas,
    LowerGet,
    UpperHas,
    UpperGet,
    FirstMutation,
    SecondMutation,
}

/// One observable boundary in the resumable Array fill algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayFillStage {
    Length,
    Set,
}

/// One observable Get boundary in the resumable Array join algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayJoinStage {
    Length,
    ElementGet,
    ElementLocaleGet,
    ElementLocaleCall,
}

/// One observable boundary in a resumable static Array constructor algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayStaticStage {
    IteratorMethod,
    IteratorCall,
    NextMethod,
    NextCall,
    ResultDone,
    ResultValue,
    Length,
    SourceValue,
    MapperCall,
    Construct,
    Define,
    FinalLength,
}

/// One observable boundary in a change-array-by-copy algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayCopyStage {
    Length,
    SourceValue,
}

/// One observable boundary in the resumable Array copyWithin algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayCopyWithinStage {
    Length,
    MoveHas,
    MoveGet,
    MoveSet,
    MoveDelete,
}

/// One observable boundary in the resumable stable Array sort machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ArrayToSortedStage {
    Length,
    SourceHas,
    SourceValue,
    CompareCall,
    WriteSet,
    WriteDelete,
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

/// Observable Get/Call stages shared by the ES2025 Set methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum SetOperationStage {
    Size,
    Has,
    Keys,
    IteratorCall,
    NextMethod,
    NextCall,
    ResultDone,
    ResultValue,
    HasCall,
    CloseReturnGetter,
    CloseReturnCall,
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
    ErrorsList,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ErrorToStringStage {
    NameValue,
    MessageValue,
}

/// Observable steps in SetterThatIgnoresPrototypeProperties for Error stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ErrorStackSetterStage {
    GetOwn,
    Define,
    Set,
}

/// Property identity retained by Iterator prototype's two special setters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IteratorPrototypeSetterKey {
    Constructor,
    ToStringTag,
}

/// Observable steps in Iterator's SetterThatIgnoresPrototypeProperties operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IteratorPrototypeSetterStage {
    GetOwn,
    Define,
    Set,
}

/// Observable boundaries in `Iterator.from` and its valid-iterator wrapper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IteratorFromStage {
    IteratorMethodGet,
    IteratorMethodCall,
    NextGet,
    HasInstance,
}

/// Observable GetIterator boundaries used before `Math.sumPrecise` enters its eager driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum MathSumPreciseStage {
    IteratorMethodGet,
    IteratorMethodCall,
}

/// Observable boundaries in `%WrapForValidIteratorPrototype%` methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum WrapForValidIteratorStage {
    NextCall,
    ReturnGet,
    ReturnCall,
}

/// Observable boundaries in lazy Iterator Helper creation, stepping, and closing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IteratorHelperStage {
    CreateMapNextGet,
    CreateFilterNextGet,
    CreateFlatMapNextGet,
    CreateTakeLimitConversion,
    CreateDropLimitConversion,
    CreateTakeNextGet,
    CreateDropNextGet,
    CreateCloseReturnGet,
    CreateCloseReturnCall,
    NextCall,
    DoneGet,
    ValueGet,
    CallbackCall,
    FlatMapIteratorMethodGet,
    FlatMapIteratorMethodCall,
    FlatMapNextGet,
    InnerNextCall,
    InnerDoneGet,
    InnerValueGet,
    InnerCloseReturnGet,
    InnerCloseReturnCall,
    AbruptCloseReturnGet,
    AbruptCloseReturnCall,
    NormalCloseReturnGet,
    NormalCloseReturnCall,
}

/// Observable boundaries shared by the eager Iterator Helper terminal operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IteratorEagerStage {
    NextGet,
    NextCall,
    DoneGet,
    ValueGet,
    CallbackCall,
    ThrowCloseReturnGet,
    ThrowCloseReturnCall,
    NormalCloseReturnGet,
    NormalCloseReturnCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ObjectToLocaleStringStage {
    Get,
    Call,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum DateToJsonStage {
    Get,
    Call,
}

/// Observable callback boundaries in String.prototype.split.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StringSplitStage {
    SplitterGet,
    SplitterCall,
}

/// Observable property-read boundaries in `String.raw`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StringRawStage {
    Raw,
    Length,
    Element,
}

/// Observable boundaries in `String.prototype.replaceAll`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StringReplaceAllStage {
    MatchGet,
    FlagsGet,
    ReplaceGet,
    ReplaceCall,
}

/// Observable boundaries shared by String match and matchAll protocols.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StringMatchStage {
    IsRegExpMatchGet,
    FlagsGet,
    MethodGet,
    MethodCall,
    CreatedMethodGet,
    CreatedMethodCall,
}

/// Observable IsRegExp boundary in String containment methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StringPrototypeStage {
    MatchGet,
}

/// Observable callback boundaries in `RegExp.prototype.test`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum RegExpTestStage {
    ExecGet,
    ExecCall,
    LastIndexGet,
    LastIndexSet,
}

/// Observable boundaries shared by String and RegExp search protocols.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum RegExpSearchStage {
    StringMethodGet,
    StringMethodCall,
    StringCreatedMethodGet,
    StringCreatedMethodCall,
    PreviousLastIndexGet,
    ZeroLastIndexSet,
    ExecGet,
    ExecCall,
    BuiltinLastIndexSet,
    CurrentLastIndexGet,
    RestoreLastIndexSet,
    ResultIndexGet,
}

/// Observable boundaries in `%RegExpStringIteratorPrototype%.next`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum RegExpStringIteratorStage {
    ExecGet,
    ExecCall,
    MatchGet,
    LastIndexGet,
    LastIndexSet,
}

/// Observable callback boundaries in source-based TypedArray construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypedArrayConstructionStage {
    Prototype,
    IteratorMethod,
    SourceList,
    ArrayLikeLength,
    ArrayLikeElement,
}

/// Observable property-read boundaries in `%TypedArray.prototype.set%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypedArraySetStage {
    Length,
    Element,
}

/// Observable species boundaries in fixed Number `%TypedArray.prototype.slice%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypedArraySliceStage {
    Constructor,
    Species,
    Construct,
}

/// Observable species boundaries in fixed Number `%TypedArray.prototype.subarray%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypedArraySubarrayStage {
    Constructor,
    Species,
    Construct,
}

/// Observable callback and species boundaries in TypedArray map/filter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypedArrayTransformStage {
    Constructor,
    Species,
    Construct,
    Callback,
}

/// Observable callback boundaries in `Signal.State` construction and mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum SignalStateStage {
    OptionsEquals,
    OptionsWatched,
    OptionsUnwatched,
    ComputedOptionsEquals,
    Equals,
}

/// Observable suspension points in the iterative JSON serialization pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum JsonStringifyStage {
    ValueGet,
    ToJsonGet,
    ToJsonCall,
    ReplacerCall,
    ReplacerLengthGet,
    ReplacerElementGet,
    ArrayLengthGet,
    ObjectKeys,
    ObjectDescriptor,
}

/// Observable boundaries in `CanonicalizeLocaleList`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IntlLocaleListStage {
    Length,
    Has,
    Get,
}

/// Observable boundaries in `Intl.Collator` construction and locale filtering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum IntlCollatorStage {
    Locales,
    Usage,
    LocaleMatcher,
    Collation,
    Numeric,
    CaseFirst,
    Sensitivity,
    IgnorePunctuation,
}

/// Observable boundaries in the Async-from-Sync iterator algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum AsyncFromSyncIteratorStage {
    IteratorCall,
    ReturnGet,
    ReturnCall,
    ThrowGet,
    ThrowCall,
    MissingThrowReturnGet,
    MissingThrowReturnCall,
    DoneGet,
    ValueGet,
    PromiseConstructorGet,
    PromiseResolve,
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
    SetOperation(SetOperationStage),
    CopyDataProperties(CopyDataPropertiesStage),
    DefineProperties(DefinePropertiesStage),
    GetOwnPropertyDescriptors(GetOwnPropertyDescriptorsStage),
    CollectionForEach,
    ArrayForEach(ArrayForEachStage),
    TypedArrayCallback(TypedArrayCallbackStage),
    TypedArrayTransform(TypedArrayTransformStage),
    ArrayConcat(ArrayConcatStage),
    ArrayFlat(ArrayFlatStage),
    ArrayFlatMap(ArrayFlatMapStage),
    ArrayCopy(ArrayCopyStage),
    ArrayCopyWithin(ArrayCopyWithinStage),
    ArrayToSorted(ArrayToSortedStage),
    ArraySlice(ArraySliceStage),
    ArrayBufferSlice(ArrayBufferSliceStage),
    SharedArrayBufferConstructor(SharedArrayBufferConstructorStage),
    ArraySplice(ArraySpliceStage),
    ArrayRemove(ArrayRemoveStage),
    ArrayInsert(ArrayInsertStage),
    ArrayReverse(ArrayReverseStage),
    ArrayFill(ArrayFillStage),
    ArrayJoin(ArrayJoinStage),
    ArrayStatic(ArrayStaticStage),
    MapGetOrInsertComputed,
    InstanceElements(InstanceElementStage),
    InstanceOf(InstanceOfStage),
    ErrorConstructor(ErrorConstructorStage),
    ErrorToString(ErrorToStringStage),
    ErrorStackSetter(ErrorStackSetterStage),
    IteratorPrototypeSetter {
        key: IteratorPrototypeSetterKey,
        stage: IteratorPrototypeSetterStage,
    },
    IteratorFrom(IteratorFromStage),
    MathSumPrecise(MathSumPreciseStage),
    IteratorHelper(IteratorHelperStage),
    IteratorEager(IteratorEagerStage),
    WrapForValidIterator(WrapForValidIteratorStage),
    DynamicFunctionPrototype,
    ObjectToString,
    ObjectIsPrototypeOf,
    ObjectLookupAccessor {
        stage: ObjectLookupAccessorStage,
        setter: bool,
    },
    ObjectToLocaleString(ObjectToLocaleStringStage),
    DateToJson(DateToJsonStage),
    RegExpTest(RegExpTestStage),
    RegExpSearch(RegExpSearchStage),
    RegExpStringIterator(RegExpStringIteratorStage),
    RegExpReplace,
    RegExpFlags(u8),
    StringSplit(StringSplitStage),
    StringRaw(StringRawStage),
    StringReplaceAll(StringReplaceAllStage),
    StringMatch(StringMatchStage),
    StringPrototype(StringPrototypeStage),
    TypedArrayConstruction(TypedArrayConstructionStage),
    TypedArraySet(TypedArraySetStage),
    TypedArraySlice(TypedArraySliceStage),
    TypedArraySubarray(TypedArraySubarrayStage),
    IntlLocaleList(IntlLocaleListStage),
    IntlCollator(IntlCollatorStage),
    JsonStringify(JsonStringifyStage),
    JsonParseReviver,
    SignalState(SignalStateStage),
    SignalWatcherHook,
    SignalComputed,
    SignalUntrack,
    GeneratorInitialize,
    GeneratorResume,
    AsyncFunction,
    AsyncModule,
    AsyncAwaitConstructor,
    AsyncFromSyncIterator(AsyncFromSyncIteratorStage),
    AsyncFromSyncCloseOnReject(CollectionIteratorCloseStage),
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
    PromiseCombinator(PromiseCombinatorStage),
    PromiseResolution(PromiseResolutionMode),
    PromiseThenable,
    FinalizationCleanup,
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
    /// Roots Collator constructor state while one ordinary option getter executes JavaScript.
    #[inline]
    pub(crate) const fn intl_collator_property_get(
        site: NativeContinuationSite,
        state: Value,
        options: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PropertyGet(PropertyCallbackMode::IntlCollator),
            first: state,
            second: options,
        }
    }

    /// Roots a pending Collator while locale canonicalization or option access executes JS.
    #[inline]
    pub(crate) const fn intl_collator(
        site: NativeContinuationSite,
        stage: IntlCollatorStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::IntlCollator(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots the locale-list state across one observable Get or HasProperty operation.
    #[inline]
    pub(crate) const fn intl_locale_list(
        site: NativeContinuationSite,
        stage: IntlLocaleListStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::IntlLocaleList(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots an unpublished generator while its parameter prologue runs synchronously.
    #[inline]
    pub(crate) const fn generator_initialize(
        site: NativeContinuationSite,
        generator: Value,
        callee: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::GeneratorInitialize,
            first: generator,
            second: callee,
        }
    }

    /// Roots the complete GC-owned JSON operation across one observable boundary.
    #[inline]
    pub(crate) const fn json_stringify(
        site: NativeContinuationSite,
        stage: JsonStringifyStage,
        state: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::JsonStringify(stage),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Retains the reviver and root wrapper across one JSON.parse callback.
    #[inline]
    pub(crate) const fn json_parse_reviver(
        site: NativeContinuationSite,
        reviver: Value,
        wrapper: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::JsonParseReviver,
            first: reviver,
            second: wrapper,
        }
    }

    /// Roots the generator instance while its explicit bytecode frame executes.
    #[inline]
    pub(crate) const fn generator_resume(site: NativeContinuationSite, generator: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::GeneratorResume,
            first: generator,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots an async generator and its active request Promise while its Fiber executes.
    #[inline]
    pub(crate) const fn async_generator_resume(
        site: NativeContinuationSite,
        generator: Value,
        promise: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::GeneratorResume,
            first: generator,
            second: promise,
        }
    }

    /// Roots one async-function state while its bytecode Fiber is active.
    #[inline]
    pub(crate) const fn async_function(site: NativeContinuationSite, state: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::AsyncFunction,
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots one module-owned execution state while its bytecode Fiber is active.
    #[inline]
    pub(crate) const fn async_module(site: NativeContinuationSite, state: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::AsyncModule,
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots an async function and awaited native Promise across its constructor getter.
    #[inline]
    pub(crate) const fn async_await_constructor(
        site: NativeContinuationSite,
        state: Value,
        source: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::AsyncAwaitConstructor,
            first: state,
            second: source,
        }
    }

    /// Roots one Async-from-Sync operation across an observable Get, Call, or resolution.
    #[inline]
    pub(crate) const fn async_from_sync_iterator(
        site: NativeContinuationSite,
        stage: AsyncFromSyncIteratorStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::AsyncFromSyncIterator(stage),
            first: state,
            second: retained,
        }
    }

    /// Retains the sync iterator and original rejection while IteratorClose runs.
    #[inline]
    pub(crate) const fn async_from_sync_close_on_reject(
        site: NativeContinuationSite,
        stage: CollectionIteratorCloseStage,
        iterator: Value,
        reason: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::AsyncFromSyncCloseOnReject(stage),
            first: iterator,
            second: reason,
        }
    }

    /// Roots State construction/mutation state across one observable getter or callback.
    #[inline]
    pub(crate) const fn signal_state(
        site: NativeContinuationSite,
        stage: SignalStateStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::SignalState(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots a pending Watcher operation while one lifecycle hook executes.
    #[inline]
    pub(crate) const fn signal_watcher_hook(
        site: NativeContinuationSite,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::SignalWatcherHook,
            first: state,
            second: receiver,
        }
    }

    #[inline]
    pub(crate) const fn signal_computed(
        site: NativeContinuationSite,
        computed: Value,
        previous: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::SignalComputed,
            first: computed,
            second: previous,
        }
    }

    /// Roots the prior dependency owner while untracked JavaScript executes.
    #[inline]
    pub(crate) const fn signal_untrack(site: NativeContinuationSite, previous: Value) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::SignalUntrack,
            first: previous,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

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

    /// Roots the boxed receiver and compact builtin-tag fallback across observable `@@toStringTag` Get.
    #[inline]
    pub(crate) const fn object_to_string(
        site: NativeContinuationSite,
        receiver: Value,
        builtin_tag: u8,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ObjectToString,
            first: receiver,
            second: Value::from_i32(builtin_tag as i32),
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
    pub(crate) const fn date_to_json(
        site: NativeContinuationSite,
        stage: DateToJsonStage,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::DateToJson(stage),
            first: receiver,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots the RegExp receiver and converted input across `exec` lookup and invocation.
    #[inline]
    pub(crate) const fn regexp_test(
        site: NativeContinuationSite,
        stage: RegExpTestStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::RegExpTest(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots the fixed search state and active receiver across one observable boundary.
    #[inline]
    pub(crate) const fn regexp_search(
        site: NativeContinuationSite,
        stage: RegExpSearchStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::RegExpSearch(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots one iterator step state across an observable Get or Call.
    #[inline]
    pub(crate) const fn regexp_string_iterator(
        site: NativeContinuationSite,
        stage: RegExpStringIteratorStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::RegExpStringIterator(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots the dedicated functional replacement state while one callback executes.
    #[inline]
    pub(crate) const fn regexp_replace(
        site: NativeContinuationSite,
        state: Value,
        replacer: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::RegExpReplace,
            first: state,
            second: replacer,
        }
    }

    /// Roots the flags getter state and receiver across one observable property read.
    #[inline]
    pub(crate) const fn regexp_flags(
        site: NativeContinuationSite,
        index: u8,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::RegExpFlags(index),
            first: state,
            second: receiver,
        }
    }

    #[inline]
    pub(crate) const fn string_split(
        site: NativeContinuationSite,
        stage: StringSplitStage,
        state: Value,
        separator: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::StringSplit(stage),
            first: state,
            second: separator,
        }
    }

    /// Roots String.raw state across one Proxy/accessor-aware property read.
    #[inline]
    pub(crate) const fn string_raw(
        site: NativeContinuationSite,
        stage: StringRawStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::StringRaw(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots the replaceAll protocol state across one observable Get or Call.
    #[inline]
    pub(crate) const fn string_replace_all(
        site: NativeContinuationSite,
        stage: StringReplaceAllStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::StringReplaceAll(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots a String match protocol state across one observable Get or Call.
    #[inline]
    pub(crate) const fn string_match(
        site: NativeContinuationSite,
        stage: StringMatchStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::StringMatch(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots a generic String operation across one observable `@@match` read.
    #[inline]
    pub(crate) const fn string_prototype(
        site: NativeContinuationSite,
        stage: StringPrototypeStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::StringPrototype(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots one TypedArray construction state across a nested observable operation.
    #[inline]
    pub(crate) const fn typed_array_construction(
        site: NativeContinuationSite,
        stage: TypedArrayConstructionStage,
        state: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::TypedArrayConstruction(stage),
            first: state,
            second: Value::from_immediate(Immediate::Undefined),
        }
    }

    /// Roots one set state and its property receiver across an observable getter.
    #[inline]
    pub(crate) const fn typed_array_set(
        site: NativeContinuationSite,
        stage: TypedArraySetStage,
        state: Value,
        receiver: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::TypedArraySet(stage),
            first: state,
            second: receiver,
        }
    }

    /// Roots one TypedArray slice state across species property access or construction.
    #[inline]
    pub(crate) const fn typed_array_slice(
        site: NativeContinuationSite,
        stage: TypedArraySliceStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::TypedArraySlice(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one TypedArray subarray state across species access or construction.
    #[inline]
    pub(crate) const fn typed_array_subarray(
        site: NativeContinuationSite,
        stage: TypedArraySubarrayStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::TypedArraySubarray(stage),
            first: state,
            second: retained,
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

    /// Roots dynamic-function state and its effective newTarget across prototype Get.
    #[inline]
    pub(crate) const fn dynamic_function_prototype(
        site: NativeContinuationSite,
        state: Value,
        new_target: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::DynamicFunctionPrototype,
            first: state,
            second: new_target,
        }
    }

    /// Roots the receiver and assigned String across one observable stack-setter step.
    #[inline]
    pub(crate) const fn error_stack_setter(
        site: NativeContinuationSite,
        stage: ErrorStackSetterStage,
        receiver: Value,
        value: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ErrorStackSetter(stage),
            first: receiver,
            second: value,
        }
    }

    /// Roots both setter operands while the property key stays in compact continuation metadata.
    #[inline]
    pub(crate) const fn iterator_prototype_setter(
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        stage: IteratorPrototypeSetterStage,
        receiver: Value,
        value: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::IteratorPrototypeSetter { key, stage },
            first: receiver,
            second: value,
        }
    }

    /// Retains the two live iterator-record operands across one `Iterator.from` boundary.
    #[inline]
    pub(crate) const fn iterator_from(
        site: NativeContinuationSite,
        stage: IteratorFromStage,
        first: Value,
        second: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::IteratorFrom(stage),
            first,
            second,
        }
    }

    /// Retains the iterable and iterator method across one GetIterator boundary.
    #[inline]
    pub(crate) const fn math_sum_precise(
        site: NativeContinuationSite,
        stage: MathSumPreciseStage,
        first: Value,
        second: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::MathSumPrecise(stage),
            first,
            second,
        }
    }

    /// Retains the helper or direct iterator plus one stage-specific rooted operand.
    #[inline]
    pub(crate) const fn iterator_helper(
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        first: Value,
        second: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::IteratorHelper(stage),
            first,
            second,
        }
    }

    /// Retains the eager operation state and one stage-specific rooted operand.
    #[inline]
    pub(crate) const fn iterator_eager(
        site: NativeContinuationSite,
        stage: IteratorEagerStage,
        first: Value,
        second: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::IteratorEager(stage),
            first,
            second,
        }
    }

    /// Retains the underlying iterator while a wrapper Get or Call executes.
    #[inline]
    pub(crate) const fn wrap_for_valid_iterator(
        site: NativeContinuationSite,
        stage: WrapForValidIteratorStage,
        iterator: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::WrapForValidIterator(stage),
            first: iterator,
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
    pub(crate) const fn instance_of(
        site: NativeContinuationSite,
        stage: InstanceOfStage,
        first: Value,
        second: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::InstanceOf(stage),
            first,
            second,
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

    /// Roots Promise constructor inputs while `newTarget.prototype` executes user code.
    #[inline]
    pub(crate) const fn promise_constructor_prototype(
        site: NativeContinuationSite,
        state: Value,
        new_target: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseStaticResolve(
                PromiseStaticResolveStage::ConstructorPrototype,
            ),
            first: state,
            second: new_target,
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

    #[inline]
    pub(crate) const fn finalization_cleanup(site: NativeContinuationSite) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::FinalizationCleanup,
            first: Value::from_immediate(Immediate::Undefined),
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

    /// Roots one Promise combinator state across an observable Get or Call.
    #[inline]
    pub(crate) const fn promise_combinator(
        site: NativeContinuationSite,
        stage: PromiseCombinatorStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::PromiseCombinator(stage),
            first: state,
            second: retained,
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

    /// Roots a Set operation while one observable protocol step executes.
    #[inline]
    pub(crate) const fn set_operation(
        site: NativeContinuationSite,
        stage: SetOperationStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::SetOperation(stage),
            first: state,
            second: retained,
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

    /// Roots one TypedArray callback state and the current element across a JS frame.
    #[inline]
    pub(crate) const fn typed_array_callback(
        site: NativeContinuationSite,
        stage: TypedArrayCallbackStage,
        state: Value,
        element: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::TypedArrayCallback(stage),
            first: state,
            second: element,
        }
    }

    /// Roots one TypedArray map/filter state across callback, species, and construction frames.
    #[inline]
    pub(crate) const fn typed_array_transform(
        site: NativeContinuationSite,
        stage: TypedArrayTransformStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::TypedArrayTransform(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one splice state and operation-specific value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_splice(
        site: NativeContinuationSite,
        stage: ArraySpliceStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArraySplice(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one pop/shift state and retained value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_remove(
        site: NativeContinuationSite,
        stage: ArrayRemoveStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayRemove(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one push/unshift state and retained value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_insert(
        site: NativeContinuationSite,
        stage: ArrayInsertStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayInsert(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one reverse state and retained value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_reverse(
        site: NativeContinuationSite,
        stage: ArrayReverseStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayReverse(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one fill state and retained value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_fill(
        site: NativeContinuationSite,
        stage: ArrayFillStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayFill(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one join state and retained value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_join(
        site: NativeContinuationSite,
        stage: ArrayJoinStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayJoin(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one slice state and operation-specific value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_slice(
        site: NativeContinuationSite,
        stage: ArraySliceStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArraySlice(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one fixed ArrayBuffer slice state across property access or construction.
    #[inline]
    pub(crate) const fn array_buffer_slice(
        site: NativeContinuationSite,
        stage: ArrayBufferSliceStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayBufferSlice(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots SharedArrayBuffer constructor state across one observable property read.
    #[inline]
    pub(crate) const fn shared_array_buffer_constructor(
        site: NativeContinuationSite,
        stage: SharedArrayBufferConstructorStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::SharedArrayBufferConstructor(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one change-array-by-copy state across an observable operation.
    #[inline]
    pub(crate) const fn array_copy(
        site: NativeContinuationSite,
        stage: ArrayCopyStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayCopy(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one copyWithin state and retained value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_copy_within(
        site: NativeContinuationSite,
        stage: ArrayCopyWithinStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayCopyWithin(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one stable-sort state across a property operation or comparator call.
    #[inline]
    pub(crate) const fn array_to_sorted(
        site: NativeContinuationSite,
        stage: ArrayToSortedStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayToSorted(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one concat state and operation-specific value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_concat(
        site: NativeContinuationSite,
        stage: ArrayConcatStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayConcat(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one flatMap state and operation-specific value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_flat_map(
        site: NativeContinuationSite,
        stage: ArrayFlatMapStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayFlatMap(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one flat state and operation-specific value across nested JavaScript work.
    #[inline]
    pub(crate) const fn array_flat(
        site: NativeContinuationSite,
        stage: ArrayFlatStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayFlat(stage),
            first: state,
            second: retained,
        }
    }

    /// Roots one static Array operation across construct and property callbacks.
    #[inline]
    pub(crate) const fn array_static(
        site: NativeContinuationSite,
        stage: ArrayStaticStage,
        state: Value,
        retained: Value,
    ) -> Self {
        Self {
            site,
            kind: NativeContinuationKind::ArrayStatic(stage),
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

/// Slow-path identity used to detect Fiber replacement even when frame depth stays unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FiberExecutionToken {
    storage: *const Frame,
    depth: usize,
    active: Option<(CodeId, FunctionId, u32)>,
}

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
    /// Stable referrer for module entry work without enlarging every hot activation frame.
    pub(crate) entry_module: Option<ModuleId>,
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
    /// Frame depths for active dynamic lexical environments, sparse across ordinary execution.
    pub(crate) lexical_environments: Vec<u32>,
    /// Persistent sloppy-eval var records, sparse across direct-eval-capable activations.
    pub(crate) eval_var_environments: Vec<EvalVarEnvironment>,
    /// Transient construct calls that cross receiver/prototype allocation safepoints.
    pub(crate) pending_construct_sites: Vec<CallSite>,
    pub(crate) registers: Vec<Value>,
    pub(crate) handlers: Vec<ActiveHandler>,
    pub(crate) completions: CompletionStack,
    pub(crate) pending_exception: Option<Value>,
}

impl Trace for Fiber {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.trace_roots(tracer);
    }
}

impl Fiber {
    /// Snapshots activation identity without rooting, allocating, or exposing frame references.
    #[inline(always)]
    pub(crate) fn execution_token(&self) -> FiberExecutionToken {
        FiberExecutionToken {
            storage: self.frames.as_ptr(),
            depth: self.frames.len(),
            active: self
                .frames
                .last()
                .map(|frame| (frame.code, frame.function, frame.base)),
        }
    }

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
        self.pending_construct_sites.trace(tracer);
        debug_assert_eq!(self.argument_objects.len(), self.frames.len());
        debug_assert_eq!(self.argument_sources.len(), self.frames.len());
        debug_assert_eq!(self.argument_callees.len(), self.frames.len());
        debug_assert!(self.derived_activations.iter().all(|activation| {
            activation.frame_depth != 0 && activation.frame_depth as usize <= self.frames.len()
        }));
        debug_assert!(self.base_class_activations.iter().all(|activation| {
            activation.frame_depth != 0 && activation.frame_depth as usize <= self.frames.len()
        }));
        debug_assert!(self.lexical_environments.iter().all(|depth| {
            *depth != 0 && usize::try_from(*depth).is_ok_and(|depth| depth <= self.frames.len())
        }));
        debug_assert!(self.eval_var_environments.iter().all(|environment| {
            environment.frame_depth != 0 && environment.frame_depth as usize <= self.frames.len()
        }));
        for frame_index in 0..self.frames.len() {
            let caller_base = frame_index
                .checked_sub(1)
                .and_then(|caller_index| self.frames.get(caller_index))
                .map(|caller| caller.base);
            let frame = &mut self.frames[frame_index];
            frame.environment.trace(tracer);
            frame.this_value.trace(tracer);
            frame.new_target.trace(tracer);
            frame.receiver_or_home_object.trace(tracer);
            frame.argument_prefix.trace(tracer);
            if let Some(return_register) = frame.return_register
                && !frame.return_continuation
            {
                debug_assert!(
                    caller_base
                        .and_then(|base| base.checked_add(return_register.index()))
                        .is_some_and(|index| (index as usize) < self.registers.len())
                );
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
