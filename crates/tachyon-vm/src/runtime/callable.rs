//! Callable payloads, native functions, and VM descriptor identities.

use super::super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
                | Self::NumberConstructor
                | Self::BooleanConstructor
                | Self::FunctionConstructor
                | Self::ErrorConstructor(_)
                | Self::ArrayConstructor
        )
    }

    #[inline(always)]
    pub(crate) const fn length(self) -> i32 {
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
    pub(crate) const fn name(self) -> &'static str {
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
    pub(crate) const ALL: [Self; 5] = [
        Self::Error,
        Self::Reference,
        Self::Syntax,
        Self::Type,
        Self::Range,
    ];

    #[inline(always)]
    pub(crate) const fn index(self) -> usize {
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
}

impl ObjectReceiver {
    #[inline(always)]
    pub(crate) fn value(self) -> Value {
        match self {
            Self::Ordinary(object) => Value::from_heap_ref(object.raw()),
            Self::Array(array) => Value::from_heap_ref(array.raw()),
            Self::Function(function) => Value::from_heap_ref(function.raw()),
            Self::Number(number) => Value::from_heap_ref(number.raw()),
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
    pub(crate) bound_function: GcType<BoundFunctionData>,
    pub(crate) environment: GcType<Environment>,
    pub(crate) for_in_iterator: GcType<ForInIterator>,
    pub(crate) function: GcType<FunctionObject>,
    pub(crate) number_object: GcType<NumberObject>,
    pub(crate) ordinary_object: GcType<OrdinaryObject>,
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
    pub(crate) undefined: AtomId,
    pub(crate) nan: AtomId,
    pub(crate) infinity: AtomId,
    pub(crate) errors: [AtomId; NativeErrorKind::ALL.len()],
    pub(crate) array: AtomId,
    pub(crate) object: AtomId,
    pub(crate) string: AtomId,
    pub(crate) symbol: AtomId,
    pub(crate) number: AtomId,
    pub(crate) boolean: AtomId,
    pub(crate) function: AtomId,
    pub(crate) math: AtomId,
}

impl RealmIntrinsicAtoms {
    pub(crate) const BINDING_COUNT: usize = 11 + NativeErrorKind::ALL.len();

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
        | ExecutionError::NotObject(_) => Some(NativeErrorKind::Type),
        ExecutionError::GlobalLexicalRedeclaration(_)
        | ExecutionError::GlobalLexicalAlreadyInitialized(_)
        | ExecutionError::EnvironmentBindingAlreadyInitialized { .. } => {
            Some(NativeErrorKind::Syntax)
        }
        ExecutionError::InvalidNumberRadix(_) | ExecutionError::InvalidNumberPrecision(_) => {
            Some(NativeErrorKind::Range)
        }
        _ => None,
    }
}
