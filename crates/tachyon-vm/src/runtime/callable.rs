//! Callable payloads, native functions, and VM descriptor identities.

use super::super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "native entries are installed by staged realm batches"
    )
)]
pub(crate) enum NativeFunction {
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
    StringCharAt,
    StringCharCodeAt,
    StringAt,
    StringCodePointAt,
    StringFromCharCode,
    StringFromCodePoint,
    StringToString,
    StringValueOf,
    StringIsWellFormed,
    StringToWellFormed,
    StringSlice,
    StringSubstring,
    StringIndexOf,
    StringIncludes,
    StringLastIndexOf,
    StringStartsWith,
    StringEndsWith,
    StringConcat,
    StringRepeat,
    StringPadStart,
    StringPadEnd,
    StringTrim,
    StringTrimStart,
    StringTrimEnd,
    RegExpConstructor,
    RegExpExec,
    RegExpTest,
    RegExpToString,
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
    FunctionPrototypeApply,
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
    ArrayValues,
    ArrayIteratorNext,
    IteratorIdentity,
    MapConstructor,
    MapGet,
    MapSet,
    MapHas,
    MapDelete,
    MapClear,
    MapSize,
    MapKeys,
    MapValues,
    MapEntries,
    MapForEach,
    MapGetOrInsert,
    CollectionIteratorNext,
    SetConstructor,
    SetAdd,
    SetHas,
    SetDelete,
    SetClear,
    SetSize,
    SetValues,
    SetEntries,
    SetForEach,
    WeakMapConstructor,
    WeakMapGet,
    WeakMapSet,
    WeakMapHas,
    WeakMapDelete,
    WeakSetConstructor,
    WeakSetAdd,
    WeakSetHas,
    WeakSetDelete,
    JsonParse,
    JsonStringify,
    MathAbs,
    MathAcos,
    MathAcosh,
    MathAsin,
    MathAsinh,
    MathAtan,
    MathAtanh,
    MathAtan2,
    MathCbrt,
    MathCeil,
    MathClz32,
    MathCos,
    MathCosh,
    MathExp,
    MathExpm1,
    MathFloor,
    MathF16Round,
    MathFround,
    MathHypot,
    MathImul,
    MathLog,
    MathLog1p,
    MathLog10,
    MathLog2,
    MathMax,
    MathMin,
    MathPow,
    MathRandom,
    MathRound,
    MathSign,
    MathSin,
    MathSinh,
    MathSqrt,
    MathTan,
    MathTanh,
    MathTrunc,
    GlobalIsFinite,
    GlobalIsNaN,
    GlobalParseFloat,
    GlobalParseInt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum GlobalNumberFunction {
    IsFinite,
    IsNaN,
    ParseFloat,
    ParseInt,
}

impl GlobalNumberFunction {
    pub(crate) const ALL: [Self; 4] = [
        Self::IsFinite,
        Self::IsNaN,
        Self::ParseFloat,
        Self::ParseInt,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::IsFinite => "isFinite",
            Self::IsNaN => "isNaN",
            Self::ParseFloat => "parseFloat",
            Self::ParseInt => "parseInt",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        if matches!(self, Self::ParseInt) { 2 } else { 1 }
    }

    pub(crate) const fn native(self) -> NativeFunction {
        match self {
            Self::IsFinite => NativeFunction::GlobalIsFinite,
            Self::IsNaN => NativeFunction::GlobalIsNaN,
            Self::ParseFloat => NativeFunction::GlobalParseFloat,
            Self::ParseInt => NativeFunction::GlobalParseInt,
        }
    }

    #[allow(dead_code, reason = "used when intrinsic installation is table-driven")]
    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum MathFunction {
    Abs,
    Acos,
    Acosh,
    Asin,
    Asinh,
    Atan,
    Atanh,
    Atan2,
    Cbrt,
    Ceil,
    Clz32,
    Cos,
    Cosh,
    Exp,
    Expm1,
    Floor,
    F16Round,
    Fround,
    Hypot,
    Imul,
    Log,
    Log1p,
    Log10,
    Log2,
    Max,
    Min,
    Pow,
    Random,
    Round,
    Sign,
    Sin,
    Sinh,
    Sqrt,
    Tan,
    Tanh,
    Trunc,
}

impl MathFunction {
    pub(crate) const ALL: [Self; 36] = [
        Self::Abs,
        Self::Acos,
        Self::Acosh,
        Self::Asin,
        Self::Asinh,
        Self::Atan,
        Self::Atanh,
        Self::Atan2,
        Self::Cbrt,
        Self::Ceil,
        Self::Clz32,
        Self::Cos,
        Self::Cosh,
        Self::Exp,
        Self::Expm1,
        Self::Floor,
        Self::F16Round,
        Self::Fround,
        Self::Hypot,
        Self::Imul,
        Self::Log,
        Self::Log1p,
        Self::Log10,
        Self::Log2,
        Self::Max,
        Self::Min,
        Self::Pow,
        Self::Random,
        Self::Round,
        Self::Sign,
        Self::Sin,
        Self::Sinh,
        Self::Sqrt,
        Self::Tan,
        Self::Tanh,
        Self::Trunc,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Abs => "abs",
            Self::Acos => "acos",
            Self::Acosh => "acosh",
            Self::Asin => "asin",
            Self::Asinh => "asinh",
            Self::Atan => "atan",
            Self::Atanh => "atanh",
            Self::Atan2 => "atan2",
            Self::Cbrt => "cbrt",
            Self::Ceil => "ceil",
            Self::Clz32 => "clz32",
            Self::Cos => "cos",
            Self::Cosh => "cosh",
            Self::Exp => "exp",
            Self::Expm1 => "expm1",
            Self::Floor => "floor",
            Self::F16Round => "f16round",
            Self::Fround => "fround",
            Self::Hypot => "hypot",
            Self::Imul => "imul",
            Self::Log => "log",
            Self::Log1p => "log1p",
            Self::Log10 => "log10",
            Self::Log2 => "log2",
            Self::Max => "max",
            Self::Min => "min",
            Self::Pow => "pow",
            Self::Random => "random",
            Self::Round => "round",
            Self::Sign => "sign",
            Self::Sin => "sin",
            Self::Sinh => "sinh",
            Self::Sqrt => "sqrt",
            Self::Tan => "tan",
            Self::Tanh => "tanh",
            Self::Trunc => "trunc",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Random => 0,
            Self::Atan2 | Self::Hypot | Self::Imul | Self::Max | Self::Min | Self::Pow => 2,
            _ => 1,
        }
    }

    pub(crate) const fn native(self) -> NativeFunction {
        use MathFunction as M;
        match self {
            M::Abs => NativeFunction::MathAbs,
            M::Acos => NativeFunction::MathAcos,
            M::Acosh => NativeFunction::MathAcosh,
            M::Asin => NativeFunction::MathAsin,
            M::Asinh => NativeFunction::MathAsinh,
            M::Atan => NativeFunction::MathAtan,
            M::Atanh => NativeFunction::MathAtanh,
            M::Atan2 => NativeFunction::MathAtan2,
            M::Cbrt => NativeFunction::MathCbrt,
            M::Ceil => NativeFunction::MathCeil,
            M::Clz32 => NativeFunction::MathClz32,
            M::Cos => NativeFunction::MathCos,
            M::Cosh => NativeFunction::MathCosh,
            M::Exp => NativeFunction::MathExp,
            M::Expm1 => NativeFunction::MathExpm1,
            M::Floor => NativeFunction::MathFloor,
            M::F16Round => NativeFunction::MathF16Round,
            M::Fround => NativeFunction::MathFround,
            M::Hypot => NativeFunction::MathHypot,
            M::Imul => NativeFunction::MathImul,
            M::Log => NativeFunction::MathLog,
            M::Log1p => NativeFunction::MathLog1p,
            M::Log10 => NativeFunction::MathLog10,
            M::Log2 => NativeFunction::MathLog2,
            M::Max => NativeFunction::MathMax,
            M::Min => NativeFunction::MathMin,
            M::Pow => NativeFunction::MathPow,
            M::Random => NativeFunction::MathRandom,
            M::Round => NativeFunction::MathRound,
            M::Sign => NativeFunction::MathSign,
            M::Sin => NativeFunction::MathSin,
            M::Sinh => NativeFunction::MathSinh,
            M::Sqrt => NativeFunction::MathSqrt,
            M::Tan => NativeFunction::MathTan,
            M::Tanh => NativeFunction::MathTanh,
            M::Trunc => NativeFunction::MathTrunc,
        }
    }

    #[allow(dead_code, reason = "used when intrinsic installation is table-driven")]
    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

pub(crate) enum FlatWork {
    Value(Value, u32),
    Hole,
}

impl NativeFunction {
    #[inline(always)]
    pub(crate) const fn is_constructor(self) -> bool {
        matches!(
            self,
            Self::ObjectConstructor
                | Self::StringConstructor
                | Self::RegExpConstructor
                | Self::NumberConstructor
                | Self::BooleanConstructor
                | Self::FunctionConstructor
                | Self::ErrorConstructor(_)
                | Self::ArrayConstructor
                | Self::MapConstructor
                | Self::SetConstructor
                | Self::WeakMapConstructor
                | Self::WeakSetConstructor
        )
    }

    #[inline(always)]
    pub(crate) const fn length(self) -> i32 {
        if let Some(function) = self.math_function() {
            return function.length();
        }
        if let Some(function) = self.global_number_function() {
            return function.length();
        }
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
            | Self::RegExpConstructor
            | Self::RegExpExec
            | Self::RegExpTest
            | Self::RegExpToString
            | Self::StringCharAt
            | Self::StringCharCodeAt
            | Self::StringAt
            | Self::StringCodePointAt
            | Self::StringFromCharCode
            | Self::StringFromCodePoint
            | Self::StringToString
            | Self::StringValueOf
            | Self::StringIsWellFormed
            | Self::StringToWellFormed
            | Self::StringSlice
            | Self::StringSubstring
            | Self::StringIndexOf
            | Self::StringIncludes
            | Self::StringLastIndexOf
            | Self::StringStartsWith
            | Self::StringEndsWith
            | Self::StringConcat
            | Self::StringRepeat
            | Self::StringPadStart
            | Self::StringPadEnd
            | Self::StringTrim
            | Self::StringTrimStart
            | Self::StringTrimEnd
            | Self::NumberConstructor
            | Self::BooleanConstructor
            | Self::FunctionPrototypeCall
            | Self::FunctionPrototypeApply
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
            Self::MapConstructor
            | Self::SetConstructor
            | Self::WeakMapConstructor
            | Self::WeakSetConstructor => 0,
            Self::MapGet
            | Self::MapSet
            | Self::MapHas
            | Self::MapDelete
            | Self::MapForEach
            | Self::SetAdd
            | Self::SetHas
            | Self::SetDelete
            | Self::SetForEach => 1,
            Self::WeakMapGet
            | Self::WeakMapSet
            | Self::WeakMapHas
            | Self::WeakMapDelete
            | Self::WeakSetAdd
            | Self::WeakSetHas
            | Self::WeakSetDelete => 1,
            Self::MapGetOrInsert => 2,
            Self::MapClear | Self::MapSize | Self::SetClear | Self::SetSize => 0,
            Self::MapKeys
            | Self::MapValues
            | Self::MapEntries
            | Self::CollectionIteratorNext
            | Self::SetValues
            | Self::SetEntries => 0,
            Self::NumberIsNaN
            | Self::NumberIsFinite
            | Self::NumberIsInteger
            | Self::NumberIsSafeInteger
            | Self::NumberToExponential
            | Self::NumberToFixed
            | Self::NumberToPrecision
            | Self::NumberToString => 1,
            Self::ArrayPush | Self::ArrayJoin => 1,
            Self::MathAbs
            | Self::MathAcos
            | Self::MathAcosh
            | Self::MathAsin
            | Self::MathAsinh
            | Self::MathAtan
            | Self::MathAtanh
            | Self::MathAtan2
            | Self::MathCbrt
            | Self::MathCeil
            | Self::MathClz32
            | Self::MathCos
            | Self::MathCosh
            | Self::MathExp
            | Self::MathExpm1
            | Self::MathFloor
            | Self::MathF16Round
            | Self::MathFround
            | Self::MathHypot
            | Self::MathImul
            | Self::MathLog
            | Self::MathLog1p
            | Self::MathLog10
            | Self::MathLog2
            | Self::MathMax
            | Self::MathMin
            | Self::MathPow
            | Self::MathRandom
            | Self::MathRound
            | Self::MathSign
            | Self::MathSin
            | Self::MathSinh
            | Self::MathSqrt
            | Self::MathTan
            | Self::MathTanh
            | Self::MathTrunc
            | Self::GlobalIsFinite
            | Self::GlobalIsNaN
            | Self::GlobalParseFloat
            | Self::GlobalParseInt => unreachable!(),
            Self::ObjectToString
            | Self::SymbolConstructor
            | Self::NumberValueOf
            | Self::FunctionPrototype
            | Self::ArrayToString => 0,
            Self::ArrayValues | Self::ArrayIteratorNext | Self::IteratorIdentity => 0,
            Self::JsonParse => 1,
            Self::JsonStringify => 3,
        }
    }

    #[inline]
    pub(crate) const fn name(self) -> &'static str {
        if let Some(function) = self.math_function() {
            return function.name();
        }
        if let Some(function) = self.global_number_function() {
            return function.name();
        }
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
            Self::RegExpConstructor => "RegExp",
            Self::RegExpExec => "exec",
            Self::RegExpTest => "test",
            Self::RegExpToString => "toString",
            Self::StringCharAt => "charAt",
            Self::StringCharCodeAt => "charCodeAt",
            Self::StringAt => "at",
            Self::StringCodePointAt => "codePointAt",
            Self::StringFromCharCode => "fromCharCode",
            Self::StringFromCodePoint => "fromCodePoint",
            Self::StringToString => "toString",
            Self::StringValueOf => "valueOf",
            Self::StringIsWellFormed => "isWellFormed",
            Self::StringToWellFormed => "toWellFormed",
            Self::StringSlice => "slice",
            Self::StringSubstring => "substring",
            Self::StringIndexOf => "indexOf",
            Self::StringIncludes => "includes",
            Self::StringLastIndexOf => "lastIndexOf",
            Self::StringStartsWith => "startsWith",
            Self::StringEndsWith => "endsWith",
            Self::StringConcat => "concat",
            Self::StringRepeat => "repeat",
            Self::StringPadStart => "padStart",
            Self::StringPadEnd => "padEnd",
            Self::StringTrim => "trim",
            Self::StringTrimStart => "trimStart",
            Self::StringTrimEnd => "trimEnd",
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
            Self::FunctionPrototypeApply => "apply",
            Self::FunctionPrototypeBind => "bind",
            Self::FunctionConstructor => "Function",
            Self::ErrorConstructor(NativeErrorKind::Error) => "Error",
            Self::ErrorConstructor(NativeErrorKind::Eval) => "EvalError",
            Self::ErrorConstructor(NativeErrorKind::Reference) => "ReferenceError",
            Self::ErrorConstructor(NativeErrorKind::Syntax) => "SyntaxError",
            Self::ErrorConstructor(NativeErrorKind::Type) => "TypeError",
            Self::ErrorConstructor(NativeErrorKind::Range) => "RangeError",
            Self::ErrorConstructor(NativeErrorKind::Uri) => "URIError",
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
            Self::ArrayValues => "values",
            Self::ArrayIteratorNext => "next",
            Self::IteratorIdentity => "[Symbol.iterator]",
            Self::MapConstructor => "Map",
            Self::MapGet => "get",
            Self::MapSet => "set",
            Self::MapHas => "has",
            Self::MapDelete => "delete",
            Self::MapClear => "clear",
            Self::MapSize => "get size",
            Self::MapKeys => "keys",
            Self::MapValues => "values",
            Self::MapEntries => "entries",
            Self::MapForEach => "forEach",
            Self::MapGetOrInsert => "getOrInsert",
            Self::CollectionIteratorNext => "next",
            Self::SetConstructor => "Set",
            Self::SetAdd => "add",
            Self::SetHas => "has",
            Self::SetDelete => "delete",
            Self::SetClear => "clear",
            Self::SetSize => "get size",
            Self::SetValues => "values",
            Self::SetEntries => "entries",
            Self::SetForEach => "forEach",
            Self::WeakMapConstructor => "WeakMap",
            Self::WeakMapGet => "get",
            Self::WeakMapSet => "set",
            Self::WeakMapHas => "has",
            Self::WeakMapDelete => "delete",
            Self::WeakSetConstructor => "WeakSet",
            Self::WeakSetAdd => "add",
            Self::WeakSetHas => "has",
            Self::WeakSetDelete => "delete",
            Self::JsonParse => "parse",
            Self::JsonStringify => "stringify",
            Self::MathAbs
            | Self::MathAcos
            | Self::MathAcosh
            | Self::MathAsin
            | Self::MathAsinh
            | Self::MathAtan
            | Self::MathAtanh
            | Self::MathAtan2
            | Self::MathCbrt
            | Self::MathCeil
            | Self::MathClz32
            | Self::MathCos
            | Self::MathCosh
            | Self::MathExp
            | Self::MathExpm1
            | Self::MathFloor
            | Self::MathF16Round
            | Self::MathFround
            | Self::MathHypot
            | Self::MathImul
            | Self::MathLog
            | Self::MathLog1p
            | Self::MathLog10
            | Self::MathLog2
            | Self::MathMax
            | Self::MathMin
            | Self::MathPow
            | Self::MathRandom
            | Self::MathRound
            | Self::MathSign
            | Self::MathSin
            | Self::MathSinh
            | Self::MathSqrt
            | Self::MathTan
            | Self::MathTanh
            | Self::MathTrunc
            | Self::GlobalIsFinite
            | Self::GlobalIsNaN
            | Self::GlobalParseFloat
            | Self::GlobalParseInt => unreachable!(),
        }
    }

    pub(crate) const fn math_function(self) -> Option<MathFunction> {
        use MathFunction as M;
        Some(match self {
            Self::MathAbs => M::Abs,
            Self::MathAcos => M::Acos,
            Self::MathAcosh => M::Acosh,
            Self::MathAsin => M::Asin,
            Self::MathAsinh => M::Asinh,
            Self::MathAtan => M::Atan,
            Self::MathAtanh => M::Atanh,
            Self::MathAtan2 => M::Atan2,
            Self::MathCbrt => M::Cbrt,
            Self::MathCeil => M::Ceil,
            Self::MathClz32 => M::Clz32,
            Self::MathCos => M::Cos,
            Self::MathCosh => M::Cosh,
            Self::MathExp => M::Exp,
            Self::MathExpm1 => M::Expm1,
            Self::MathFloor => M::Floor,
            Self::MathF16Round => M::F16Round,
            Self::MathFround => M::Fround,
            Self::MathHypot => M::Hypot,
            Self::MathImul => M::Imul,
            Self::MathLog => M::Log,
            Self::MathLog1p => M::Log1p,
            Self::MathLog10 => M::Log10,
            Self::MathLog2 => M::Log2,
            Self::MathMax => M::Max,
            Self::MathMin => M::Min,
            Self::MathPow => M::Pow,
            Self::MathRandom => M::Random,
            Self::MathRound => M::Round,
            Self::MathSign => M::Sign,
            Self::MathSin => M::Sin,
            Self::MathSinh => M::Sinh,
            Self::MathSqrt => M::Sqrt,
            Self::MathTan => M::Tan,
            Self::MathTanh => M::Tanh,
            Self::MathTrunc => M::Trunc,
            _ => return None,
        })
    }

    pub(crate) const fn global_number_function(self) -> Option<GlobalNumberFunction> {
        Some(match self {
            Self::GlobalIsFinite => GlobalNumberFunction::IsFinite,
            Self::GlobalIsNaN => GlobalNumberFunction::IsNaN,
            Self::GlobalParseFloat => GlobalNumberFunction::ParseFloat,
            Self::GlobalParseInt => GlobalNumberFunction::ParseInt,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NativeErrorKind {
    Error,
    Eval,
    Reference,
    Syntax,
    Type,
    Range,
    Uri,
}

impl NativeErrorKind {
    pub(crate) const ALL: [Self; 7] = [
        Self::Error,
        Self::Eval,
        Self::Reference,
        Self::Syntax,
        Self::Type,
        Self::Range,
        Self::Uri,
    ];

    #[inline(always)]
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Eval => "EvalError",
            Self::Reference => "ReferenceError",
            Self::Syntax => "SyntaxError",
            Self::Type => "TypeError",
            Self::Range => "RangeError",
            Self::Uri => "URIError",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ErrorIntrinsic {
    pub(crate) constructor: Option<Value>,
    pub(crate) prototype: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ErrorIntrinsics {
    pub(crate) entries: [ErrorIntrinsic; NativeErrorKind::ALL.len()],
}

impl ErrorIntrinsics {
    #[inline(always)]
    pub(crate) fn get(self, kind: NativeErrorKind) -> ErrorIntrinsic {
        self.entries[kind.index()]
    }

    #[inline(always)]
    pub(crate) fn get_mut(&mut self, kind: NativeErrorKind) -> &mut ErrorIntrinsic {
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
pub(crate) enum FunctionExecutable {
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
pub(crate) struct FunctionObject {
    pub(crate) executable: FunctionExecutable,
    pub(crate) function_prototype: Option<Value>,
    pub(crate) ordinary: OrdinaryObject,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SymbolValue {
    pub(crate) serial: NonZeroU32,
    pub(crate) description: Option<Value>,
}

impl Trace for SymbolValue {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.description.trace(tracer);
    }
}

/// GC-managed accessor payload stored behind one ordinary property slot.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AccessorPair {
    pub(crate) getter: Value,
    pub(crate) setter: Value,
}

impl Trace for AccessorPair {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.getter.trace(tracer);
        self.setter.trace(tracer);
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ObjectReceiver {
    Ordinary(GcRef<OrdinaryObject>),
    Array(GcRef<ArrayObject>),
    Function(GcRef<FunctionObject>),
    Number(GcRef<NumberObject>),
    String(GcRef<StringObject>),
    RegExp(GcRef<RegExpObject>),
    Map(GcRef<MapObject>),
    Set(GcRef<SetObject>),
    WeakMap(GcRef<WeakMapObject>),
    WeakSet(GcRef<WeakSetObject>),
    ArrayIterator(GcRef<ArrayIteratorObject>),
    CollectionIterator(GcRef<CollectionIteratorObject>),
}

impl ObjectReceiver {
    #[inline(always)]
    pub(crate) fn value(self) -> Value {
        match self {
            Self::Ordinary(object) => Value::from_heap_ref(object.raw()),
            Self::Array(array) => Value::from_heap_ref(array.raw()),
            Self::Function(function) => Value::from_heap_ref(function.raw()),
            Self::Number(number) => Value::from_heap_ref(number.raw()),
            Self::String(string) => Value::from_heap_ref(string.raw()),
            Self::RegExp(regexp) => Value::from_heap_ref(regexp.raw()),
            Self::Map(map) => Value::from_heap_ref(map.raw()),
            Self::Set(set) => Value::from_heap_ref(set.raw()),
            Self::WeakMap(map) => Value::from_heap_ref(map.raw()),
            Self::WeakSet(set) => Value::from_heap_ref(set.raw()),
            Self::ArrayIterator(iterator) => Value::from_heap_ref(iterator.raw()),
            Self::CollectionIterator(iterator) => Value::from_heap_ref(iterator.raw()),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ResolvedCallTarget {
    pub(crate) code: CodeId,
    pub(crate) function: FunctionId,
    pub(crate) environment: Option<GcRef<Environment>>,
    pub(crate) kind: FunctionKind,
    pub(crate) layout: FunctionLayout,
    pub(crate) strictness: FunctionStrictness,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DataPropertyDescriptor {
    pub(crate) value: Option<Value>,
    pub(crate) writable: Option<bool>,
    pub(crate) enumerable: Option<bool>,
    pub(crate) configurable: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GenericPropertyDescriptor {
    pub(crate) enumerable: Option<bool>,
    pub(crate) configurable: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AccessorPropertyDescriptor {
    pub(crate) getter: Option<Value>,
    pub(crate) setter: Option<Value>,
    pub(crate) enumerable: Option<bool>,
    pub(crate) configurable: Option<bool>,
}

/// Closed ToPropertyDescriptor result; data and accessor fields cannot coexist in one variant.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PropertyDescriptor {
    Generic(GenericPropertyDescriptor),
    Data(DataPropertyDescriptor),
    Accessor(AccessorPropertyDescriptor),
}

impl Default for PropertyDescriptor {
    fn default() -> Self {
        Self::Generic(GenericPropertyDescriptor::default())
    }
}

impl From<DataPropertyDescriptor> for PropertyDescriptor {
    fn from(descriptor: DataPropertyDescriptor) -> Self {
        if descriptor.value.is_none() && descriptor.writable.is_none() {
            Self::Generic(GenericPropertyDescriptor {
                enumerable: descriptor.enumerable,
                configurable: descriptor.configurable,
            })
        } else {
            Self::Data(descriptor)
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CallSite {
    pub(crate) caller_base: u32,
    pub(crate) destination: u32,
    pub(crate) callee: Value,
    pub(crate) argument_base: u32,
    pub(crate) argument_prefix: Option<GcRef<BoundFunctionData>>,
    pub(crate) argument_prefix_offset: u32,
    pub(crate) argument_prefix_count: u32,
    pub(crate) argument_count: u32,
    pub(crate) this_value: Value,
    pub(crate) new_target: Value,
    pub(crate) construct_receiver: Option<Value>,
    pub(crate) call_site: WordOffset,
}

#[derive(Clone, Copy)]
pub(crate) struct BoundFunctionSnapshot {
    pub(crate) bound_target: Value,
    pub(crate) call_target: Value,
    pub(crate) bound_this: Value,
    pub(crate) argument_count: u32,
    pub(crate) length: Value,
    pub(crate) name: Value,
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
pub(crate) struct VmTypes {
    pub(crate) accessor_pair: GcType<AccessorPair>,
    pub(crate) array: GcType<ArrayObject>,
    pub(crate) array_iterator: GcType<ArrayIteratorObject>,
    pub(crate) collection_iterator: GcType<CollectionIteratorObject>,
    pub(crate) bound_function: GcType<BoundFunctionData>,
    pub(crate) environment: GcType<Environment>,
    pub(crate) exclusion_list: GcType<ExclusionList>,
    pub(crate) for_in_iterator: GcType<ForInIterator>,
    pub(crate) map_object: GcType<MapObject>,
    pub(crate) ordered_collection: GcType<OrderedCollection>,
    pub(crate) weak_collection: GcType<WeakCollection>,
    pub(crate) weak_map_object: GcType<WeakMapObject>,
    pub(crate) weak_set_object: GcType<WeakSetObject>,
    pub(crate) function: GcType<FunctionObject>,
    pub(crate) number_object: GcType<NumberObject>,
    pub(crate) string_object: GcType<StringObject>,
    pub(crate) ordinary_object: GcType<OrdinaryObject>,
    pub(crate) pending_property_descriptor: GcType<PendingPropertyDescriptor>,
    pub(crate) pending_native_property_key: GcType<PendingNativePropertyKey>,
    pub(crate) pending_copy_data_properties: GcType<PendingCopyDataProperties>,
    pub(crate) pending_collection_initializer: GcType<PendingCollectionInitializer>,
    pub(crate) pending_collection_for_each: GcType<PendingCollectionForEach>,
    pub(crate) regexp_object: GcType<RegExpObject>,
    pub(crate) set_object: GcType<SetObject>,
    pub(crate) property_storage: GcType<PropertyStorage>,
    pub(crate) string: GcType<JsString>,
    pub(crate) symbol: GcType<SymbolValue>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IntrinsicPropertyAtoms {
    pub(crate) prototype: Option<AtomId>,
    pub(crate) constructor: Option<AtomId>,
    pub(crate) message: Option<AtomId>,
    pub(crate) name: Option<AtomId>,
    pub(crate) length: Option<AtomId>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RealmIntrinsicAtoms {
    pub(crate) global_this: AtomId,
    pub(crate) undefined: AtomId,
    pub(crate) nan: AtomId,
    pub(crate) infinity: AtomId,
    pub(crate) errors: [AtomId; NativeErrorKind::ALL.len()],
    pub(crate) array: AtomId,
    pub(crate) object: AtomId,
    pub(crate) string: AtomId,
    pub(crate) regexp: AtomId,
    pub(crate) map: AtomId,
    pub(crate) set: AtomId,
    pub(crate) weak_map: AtomId,
    pub(crate) weak_set: AtomId,
    pub(crate) symbol: AtomId,
    pub(crate) number: AtomId,
    pub(crate) boolean: AtomId,
    pub(crate) function: AtomId,
    pub(crate) math: AtomId,
    pub(crate) json: AtomId,
    #[allow(dead_code, reason = "reserved for global intrinsic resolution")]
    pub(crate) global_numbers: [AtomId; GlobalNumberFunction::ALL.len()],
}

impl RealmIntrinsicAtoms {
    pub(crate) const BINDING_COUNT: usize =
        18 + NativeErrorKind::ALL.len() + GlobalNumberFunction::ALL.len();

    #[inline(always)]
    pub(crate) fn error(self, kind: NativeErrorKind) -> AtomId {
        self.errors[kind.index()]
    }
}

#[inline(always)]
pub(crate) fn execution_error_kind(error: &ExecutionError) -> Option<NativeErrorKind> {
    match error {
        ExecutionError::UnresolvedBinding(_)
        | ExecutionError::UninitializedBinding(_)
        | ExecutionError::UninitializedEnvironmentBinding { .. } => {
            Some(NativeErrorKind::Reference)
        }
        ExecutionError::NonCallable(_)
        | ExecutionError::NonConstructor(_)
        | ExecutionError::InvalidInstanceofPrototype(_)
        | ExecutionError::ReadOnlyBinding(_)
        | ExecutionError::ImmutableBinding(_)
        | ExecutionError::ImmutableEnvironmentBinding { .. }
        | ExecutionError::NonExtensibleObject(_)
        | ExecutionError::ReadOnlyProperty(_)
        | ExecutionError::InvalidPropertyDescriptor(_)
        | ExecutionError::InvalidPropertyRedefinition(_)
        | ExecutionError::ArrayLengthOverflow
        | ExecutionError::NotObject(_)
        | ExecutionError::IncompatibleCollectionReceiver(_)
        | ExecutionError::InvalidJsonCircularStructure => Some(NativeErrorKind::Type),
        ExecutionError::GlobalLexicalRedeclaration(_)
        | ExecutionError::GlobalLexicalAlreadyInitialized(_)
        | ExecutionError::EnvironmentBindingAlreadyInitialized { .. }
        | ExecutionError::InvalidJsonText => Some(NativeErrorKind::Syntax),
        ExecutionError::InvalidRegExpFlags | ExecutionError::InvalidRegExpPattern => {
            Some(NativeErrorKind::Syntax)
        }
        ExecutionError::InvalidNumberRadix(_)
        | ExecutionError::InvalidNumberPrecision(_)
        | ExecutionError::InvalidStringLength
        | ExecutionError::InvalidStringRepeatCount(_) => Some(NativeErrorKind::Range),
        _ => None,
    }
}
