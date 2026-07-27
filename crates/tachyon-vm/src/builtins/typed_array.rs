//! Fixed-buffer TypedArray construction and integer-indexed element access.

mod at;
mod callback;
mod copy_within;
mod fill;
mod includes;
mod join;
mod reverse;
mod search;
mod set;
mod slice;
mod sort;
mod subarray;
mod to_reversed;
mod to_sorted;
mod with;

use super::super::*;
use super::data_view::{data_view_decode, data_view_encode};
use crate::conversion::parse_number_code_units;
use crate::iterator::ArrayIterationKind;
use crate::object::{
    ArrayBufferData, ContentType, TypedArrayKind, TypedArrayObject, ViewLengthMode,
};
use crate::property::array_index;
use crate::runtime::callable::{
    DataViewElement, TypedArrayCallbackKind, TypedArrayGetter, TypedArraySearchDirection,
};

#[derive(Clone, Copy)]
pub(crate) struct TypedArraySnapshot {
    pub(crate) buffer: Value,
    pub(crate) byte_offset: usize,
    pub(crate) length: usize,
    pub(crate) kind: TypedArrayKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedArrayIndex {
    NonNumeric,
    Invalid,
    Valid(usize),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum TypedArraySearchNeedle {
    Number(f64),
    BigInt(u64),
}

/// GC-owned state for source collection and resumable element conversion.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingTypedArrayConstruction {
    source: Value,
    target: Value,
    new_target: Value,
    prototype: Value,
    retained: Value,
    byte_offset_argument: Value,
    length_argument: Value,
    kind: TypedArrayKind,
    mode: TypedArrayConstructionMode,
    byte_offset: u64,
    index: u64,
    length: u64,
}

impl Trace for PendingTypedArrayConstruction {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.source.trace(tracer);
        self.target.trace(tracer);
        self.new_target.trace(tracer);
        self.prototype.trace(tracer);
        self.retained.trace(tracer);
        self.byte_offset_argument.trace(tracer);
        self.length_argument.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum TypedArrayConstructionMode {
    PrimitiveLength,
    ArrayBuffer,
    TypedArray,
    IterableList,
    ArrayLike,
}

#[derive(Clone, Copy)]
struct TypedArrayConstructionSnapshot {
    source: Value,
    target: Value,
    new_target: Value,
    prototype: Value,
    byte_offset_argument: Value,
    length_argument: Value,
    kind: TypedArrayKind,
    mode: TypedArrayConstructionMode,
    byte_offset: u64,
    index: u64,
    length: u64,
}

struct TypedArrayAllocationRoots<'a> {
    vm: VmRoots<'a>,
    buffer: Value,
    prototype: Value,
}

struct TypedArrayConstructionRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingTypedArrayConstruction,
}

impl Trace for TypedArrayAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.buffer.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Trace for TypedArrayConstructionRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates a fixed TypedArray before creating the shared ArrayIterator payload.
    pub(crate) fn begin_typed_array_iterator(
        &mut self,
        site: &CallSite,
        kind: ArrayIterationKind,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.typed_array_snapshot(site.this_value)?;
        self.typed_array_backing(snapshot.buffer)?;
        self.create_array_iterator(site.this_value, kind)
    }

    /// Starts fixed Number TypedArray construction for every supported source category.
    pub(crate) fn begin_typed_array_from_site(
        &mut self,
        site: &CallSite,
        kind: TypedArrayKind,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let is_object = self.is_object_value(argument);
        let mode = if !is_object {
            TypedArrayConstructionMode::PrimitiveLength
        } else if argument.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.array_buffer_object)
                .is_ok()
        }) {
            TypedArrayConstructionMode::ArrayBuffer
        } else if self.is_typed_array_value(argument) {
            TypedArrayConstructionMode::TypedArray
        } else {
            TypedArrayConstructionMode::ArrayLike
        };
        let length = if is_object {
            0
        } else {
            u64::try_from(self.ecma_to_index(argument)?)
                .map_err(|_| ExecutionError::InvalidArrayLength)?
        };
        let byte_offset_argument = self.call_argument(site, 1)?.unwrap_or(undefined);
        let length_argument = self.call_argument(site, 2)?.unwrap_or(undefined);
        let state =
            self.allocate_pending_typed_array_construction(PendingTypedArrayConstruction {
                source: argument,
                target: undefined,
                new_target: site.new_target,
                prototype: undefined,
                retained: undefined,
                byte_offset_argument,
                length_argument,
                kind,
                mode,
                byte_offset: 0,
                index: 0,
                length,
            })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_typed_array_construction(continuation_site, state)?;
        self.get_typed_array_named_property(
            continuation_site,
            state,
            TypedArrayConstructionStage::Prototype,
            site.new_target,
            b"prototype",
        )
    }

    /// Allocates one TypedArray payload while retaining its buffer and selected prototype.
    pub(crate) fn allocate_typed_array_view(
        &mut self,
        buffer: Value,
        byte_offset: usize,
        length: usize,
        kind: TypedArrayKind,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let byte_offset =
            u32::try_from(byte_offset).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let length = u32::try_from(length).map_err(|_| ExecutionError::InvalidArrayLength)?;
        let roots = &mut TypedArrayAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            buffer,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.typed_array_object,
                0,
                0,
                TypedArrayObject {
                    buffer,
                    byte_offset,
                    length,
                    kind,
                    length_mode: ViewLengthMode::Fixed,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|array| Value::from_heap_ref(array.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Routes one completed observable construction boundary to its next state.
    pub(crate) fn resume_typed_array_construction(
        &mut self,
        continuation: NativeContinuation,
        stage: TypedArrayConstructionStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.pending_typed_array_construction_reference(continuation.first())?;
        self.root_typed_array_construction(site, state)?;
        match stage {
            TypedArrayConstructionStage::Prototype => {
                self.resume_typed_array_prototype(site, state, value)
            }
            TypedArrayConstructionStage::IteratorMethod => {
                self.resume_typed_array_iterator_method(site, state, value)
            }
            TypedArrayConstructionStage::SourceList => {
                self.resume_typed_array_source_list(site, state, value)
            }
            TypedArrayConstructionStage::ArrayLikeLength => {
                self.resume_typed_array_array_like_length(site, state, value)
            }
            TypedArrayConstructionStage::ArrayLikeElement => {
                self.begin_typed_array_element_conversion(site, state, value)
            }
        }
    }

    /// Resumes ToPrimitive for ArrayBuffer indices, array-like length, or one element.
    pub(crate) fn resume_typed_array_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_typed_array_construction(site, state)?;
        match consumer {
            ConversionConsumer::TypedArrayByteOffset => {
                self.finish_typed_array_byte_offset(site, state, value)
            }
            ConversionConsumer::TypedArrayLength => {
                let mode = self.typed_array_construction_snapshot(state)?.mode;
                if mode == TypedArrayConstructionMode::ArrayBuffer {
                    self.finish_typed_array_buffer_length(site, state, value)
                } else {
                    self.finish_typed_array_array_like_length(site, state, value)
                }
            }
            ConversionConsumer::TypedArrayElement => {
                self.finish_typed_array_element_conversion(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Resolves the constructor prototype before branching on any object source brand.
    fn resume_typed_array_prototype(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.typed_array_construction_snapshot(state)?;
        let prototype = if self.is_object_value(value) {
            value
        } else {
            self.typed_array_fallback_prototype(snapshot.new_target, snapshot.kind)?
        };
        self.set_typed_array_construction_value(
            state,
            |pending| &mut pending.prototype,
            prototype,
        )?;
        match snapshot.mode {
            TypedArrayConstructionMode::PrimitiveLength => {
                self.allocate_typed_array_construction_target(site, state)
            }
            TypedArrayConstructionMode::ArrayBuffer => {
                self.begin_typed_array_byte_offset(site, state)
            }
            TypedArrayConstructionMode::TypedArray => {
                let source = self.typed_array_snapshot(snapshot.source)?;
                self.update_typed_array_construction(state, |pending| {
                    pending.length = source.length as u64;
                })?;
                self.allocate_typed_array_construction_target(site, state)
            }
            TypedArrayConstructionMode::ArrayLike | TypedArrayConstructionMode::IterableList => {
                let iterator = self
                    .realm
                    .well_known_symbols
                    .iterator
                    .expect("Symbol.iterator initializes before TypedArray");
                let key = self.property_key(iterator)?;
                self.get_typed_array_property(
                    site,
                    state,
                    TypedArrayConstructionStage::IteratorMethod,
                    snapshot.source,
                    key,
                )
            }
        }
    }

    /// Starts ArrayBuffer byteOffset conversion after prototype resolution.
    fn begin_typed_array_byte_offset(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
    ) -> Result<(), ExecutionError> {
        let value = self
            .typed_array_construction_snapshot(state)?
            .byte_offset_argument;
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::TypedArrayByteOffset,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_typed_array_byte_offset(site, state, value)
    }

    /// Stores aligned byteOffset, then converts or defaults the optional view length.
    fn finish_typed_array_byte_offset(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let offset = self.ecma_to_index(value)?;
        let width = self
            .typed_array_construction_snapshot(state)?
            .kind
            .byte_width();
        if offset % width != 0 {
            return Err(ExecutionError::InvalidArrayLength);
        }
        self.update_typed_array_construction(state, |pending| {
            pending.byte_offset = offset as u64;
        })?;
        let length = self
            .typed_array_construction_snapshot(state)?
            .length_argument;
        if length.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_typed_array_buffer_view(site, state, None);
        }
        if self.is_object_value(length) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::TypedArrayLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                length,
                site.call_site,
            );
        }
        self.finish_typed_array_buffer_length(site, state, length)
    }

    /// Normalizes an explicit ArrayBuffer view length and revalidates the current backing.
    fn finish_typed_array_buffer_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = self.ecma_to_index(value)?;
        self.finish_typed_array_buffer_view(site, state, Some(length))
    }

    /// Attaches a fixed view only after re-reading detach state and current byte length.
    fn finish_typed_array_buffer_view(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        explicit_length: Option<usize>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.typed_array_construction_snapshot(state)?;
        let raw = snapshot
            .source
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(snapshot.source))?;
        let buffer = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(snapshot.source))?;
        let buffer_length = self.array_buffer_length_for_view(buffer)?;
        let offset = usize::try_from(snapshot.byte_offset)
            .map_err(|_| ExecutionError::InvalidArrayLength)?;
        if offset > buffer_length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let length = if let Some(length) = explicit_length {
            let bytes = length
                .checked_mul(snapshot.kind.byte_width())
                .ok_or(ExecutionError::InvalidArrayLength)?;
            if bytes > buffer_length - offset {
                return Err(ExecutionError::InvalidArrayLength);
            }
            length
        } else {
            if buffer_length % snapshot.kind.byte_width() != 0 {
                return Err(ExecutionError::InvalidArrayLength);
            }
            (buffer_length - offset) / snapshot.kind.byte_width()
        };
        let auto_length =
            explicit_length.is_none() && self.array_buffer_is_resizable(snapshot.source)?;
        let target = self.allocate_typed_array_view(
            snapshot.source,
            offset,
            length,
            snapshot.kind,
            snapshot.prototype,
        )?;
        if auto_length {
            self.set_typed_array_length_mode(target, ViewLengthMode::Tracking)?;
        }
        self.write(site.caller_base, site.destination, target)
    }

    /// Chooses iterator collection or array-like indexed reads after GetMethod.
    fn resume_typed_array_iterator_method(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        method: Value,
    ) -> Result<(), ExecutionError> {
        if is_nullish(method) {
            self.update_typed_array_construction(state, |pending| {
                pending.mode = TypedArrayConstructionMode::ArrayLike;
            })?;
            let source = self.typed_array_construction_snapshot(state)?.source;
            return self.get_typed_array_named_property(
                site,
                state,
                TypedArrayConstructionStage::ArrayLikeLength,
                source,
                b"length",
            );
        }
        self.resolve_function_object(method)?;
        self.dispatch_typed_array_source_list(site, state, method)
    }

    /// Runs the shared IteratorToList state machine beneath one TypedArray parent continuation.
    fn dispatch_typed_array_source_list(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        method: Value,
    ) -> Result<(), ExecutionError> {
        let source = self.typed_array_construction_snapshot(state)?.source;
        let depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::typed_array_construction(
                site,
                TypedArrayConstructionStage::SourceList,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.begin_iterator_method_to_list(site, source, method) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth || self.fiber.completions.len() == depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_typed_array_construction(
            continuation,
            TypedArrayConstructionStage::SourceList,
            value,
        )
    }

    /// Freezes an IteratorToList result before allocating and converting the target backing.
    fn resume_typed_array_source_list(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        list: Value,
    ) -> Result<(), ExecutionError> {
        let length = self.typed_array_list_length(list)?;
        self.set_typed_array_construction_value(state, |pending| &mut pending.source, list)?;
        self.update_typed_array_construction(state, |pending| {
            pending.mode = TypedArrayConstructionMode::IterableList;
            pending.length = length;
        })?;
        self.allocate_typed_array_construction_target(site, state)
    }

    /// Converts the frozen array-like length before allocating the target backing.
    fn resume_typed_array_array_like_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::TypedArrayLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_typed_array_array_like_length(site, state, value)
    }

    /// Stores ToLength(arrayLike.length), then creates the fixed target backing once.
    fn finish_typed_array_array_like_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let converted = self.convert_to_number(value)?;
        let length = typed_array_to_length(converted)?;
        self.update_typed_array_construction(state, |pending| pending.length = length)?;
        self.allocate_typed_array_construction_target(site, state)
    }

    /// Allocates a target with the already resolved prototype and republishes moving state edges.
    fn allocate_typed_array_construction_target(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.typed_array_construction_snapshot(state)?;
        let length =
            usize::try_from(snapshot.length).map_err(|_| ExecutionError::InvalidArrayLength)?;
        if length > u32::MAX as usize / snapshot.kind.byte_width() {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let byte_length = length
            .checked_mul(snapshot.kind.byte_width())
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let array_buffer_prototype = self
            .realm
            .array_buffer_prototype
            .expect("ArrayBuffer prototype initializes before TypedArray");
        let buffer = self.allocate_array_buffer_object(
            byte_length,
            byte_length,
            false,
            array_buffer_prototype,
        )?;
        let state = self.pending_typed_array_construction_reference(
            self.read(site.caller_base, site.destination)?,
        )?;
        let snapshot = self.typed_array_construction_snapshot(state)?;
        let target =
            self.allocate_typed_array_view(buffer, 0, length, snapshot.kind, snapshot.prototype)?;
        let state = self.pending_typed_array_construction_reference(
            self.read(site.caller_base, site.destination)?,
        )?;
        self.set_typed_array_construction_value(state, |pending| &mut pending.target, target)?;
        let snapshot = self.typed_array_construction_snapshot(state)?;
        if snapshot.mode == TypedArrayConstructionMode::PrimitiveLength {
            return self.write(site.caller_base, site.destination, snapshot.target);
        }
        if snapshot.mode == TypedArrayConstructionMode::TypedArray {
            let source = self.typed_array_snapshot(snapshot.source)?;
            if source.kind.content_type() != snapshot.kind.content_type() {
                return Err(ExecutionError::TypedArrayContentTypeMismatch);
            }
            if source.kind == snapshot.kind {
                self.copy_same_kind_typed_array(snapshot.source, snapshot.target)?;
                return self.write(site.caller_base, site.destination, snapshot.target);
            }
        }
        self.advance_typed_array_construction(site, state)
    }

    /// Reads the next list/source value or dispatches one observable array-like indexed Get.
    fn advance_typed_array_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.typed_array_construction_snapshot(state)?;
            if snapshot.index >= snapshot.length {
                return self.write(site.caller_base, site.destination, snapshot.target);
            }
            let key = self.safe_integer_property_atom(snapshot.index)?;
            if snapshot.mode == TypedArrayConstructionMode::ArrayLike {
                let value =
                    match self.resolve_property_read_until_proxy(snapshot.source, key.into())? {
                        PropertyReadResolution::Read(PropertyRead::Data(value)) => value,
                        PropertyReadResolution::Read(PropertyRead::Missing) => {
                            Value::from_immediate(Immediate::Undefined)
                        }
                        PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                            if getter.as_immediate() == Some(Immediate::Undefined) =>
                        {
                            Value::from_immediate(Immediate::Undefined)
                        }
                        PropertyReadResolution::Read(PropertyRead::Accessor(_))
                        | PropertyReadResolution::Proxy(_) => {
                            return self.get_typed_array_property(
                                site,
                                state,
                                TypedArrayConstructionStage::ArrayLikeElement,
                                snapshot.source,
                                key.into(),
                            );
                        }
                    };
                if self.is_object_value(value) {
                    return self.begin_typed_array_element_conversion(site, state, value);
                }
                self.write_typed_array_construction_element(state, value)?;
                continue;
            }
            let value = self
                .get_data_property(snapshot.source, key)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            if self.is_object_value(value) {
                return self.begin_typed_array_element_conversion(site, state, value);
            }
            self.write_typed_array_construction_element(state, value)?;
        }
    }

    /// Converts one source value with Number hint while retaining it across callbacks.
    fn begin_typed_array_element_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_typed_array_construction_value(state, |pending| &mut pending.retained, value)?;
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::TypedArrayElement,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_typed_array_element_conversion(site, state, value)
    }

    /// Writes one converted element and advances only after the backing write succeeds.
    fn finish_typed_array_element_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.write_typed_array_construction_element(state, value)?;
        self.advance_typed_array_construction(site, state)
    }

    /// Converts and commits one element without recursively entering the next primitive element.
    fn write_typed_array_construction_element(
        &mut self,
        state: GcRef<PendingTypedArrayConstruction>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.typed_array_construction_snapshot(state)?;
        let target = self.typed_array_snapshot(snapshot.target)?;
        let index =
            usize::try_from(snapshot.index).map_err(|_| ExecutionError::InvalidArrayLength)?;
        self.typed_array_write_value(target, index, value)?;
        let next_index = snapshot
            .index
            .checked_add(1)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        self.update_typed_array_construction(state, |pending| {
            pending.index = next_index;
            pending.retained = Value::from_immediate(Immediate::Undefined);
        })
    }

    /// Copies a same-kind source byte-for-byte so NaN payloads and signed zero survive.
    pub(crate) fn copy_same_kind_typed_array(
        &mut self,
        source: Value,
        target: Value,
    ) -> Result<(), ExecutionError> {
        let source = self.typed_array_snapshot(source)?;
        let target = self.typed_array_snapshot(target)?;
        let width = source.kind.byte_width();
        let copy_length = source.length.min(target.length);
        let byte_length = copy_length
            .checked_mul(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let source_data = self.typed_array_backing(source.buffer)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(source_data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                bytes.extend_from_slice(
                    &data.bytes[source.byte_offset..source.byte_offset + byte_length],
                );
                Ok::<(), ExecutionError>(())
            })
        })?;
        let target_data = self.typed_array_backing(target.buffer)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(target_data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .bytes[target.byte_offset..target.byte_offset + byte_length]
                    .copy_from_slice(&bytes);
                Ok(())
            })
        })
    }

    /// Reads the intrinsic list length produced by the shared iterator collector.
    fn typed_array_list_length(&mut self, list: Value) -> Result<u64, ExecutionError> {
        let length_atom = self.length_atom()?;
        let length = self
            .get_data_property(list, length_atom)?
            .and_then(numeric_value)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
            return Err(ExecutionError::InvalidArrayLength);
        }
        Ok(length as u64)
    }

    /// Performs one Proxy/accessor-aware Get under a typed construction parent.
    fn get_typed_array_named_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        stage: TypedArrayConstructionStage,
        receiver: Value,
        name: &[u8],
    ) -> Result<(), ExecutionError> {
        let key = self.intern_intrinsic_name(name)?;
        self.get_typed_array_property(site, state, stage, receiver, key.into())
    }

    /// Dispatches one resumable property read and drains it immediately when synchronous.
    fn get_typed_array_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
        stage: TypedArrayConstructionStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::typed_array_construction(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth || self.fiber.completions.len() == depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_typed_array_construction(continuation, stage, value)
    }

    /// Returns the concrete fallback prototype from the newTarget function's Realm.
    fn typed_array_fallback_prototype(
        &mut self,
        new_target: Value,
        kind: TypedArrayKind,
    ) -> Result<Value, ExecutionError> {
        let realm = self.realm_for_callable(new_target)?;
        let prototype = if realm == self.active_realm {
            self.realm.typed_array_prototypes[kind.index()]
        } else {
            self.inactive_realms
                .iter()
                .find(|(id, _)| *id == realm)
                .and_then(|(_, realm)| realm.typed_array_prototypes[kind.index()])
        };
        prototype.ok_or(ExecutionError::MissingNativeContinuation)
    }

    /// Allocates the fixed construction state under the complete VM root set.
    fn allocate_pending_typed_array_construction(
        &mut self,
        pending: PendingTypedArrayConstruction,
    ) -> Result<GcRef<PendingTypedArrayConstruction>, ExecutionError> {
        let mut roots = TypedArrayConstructionRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_typed_array_construction,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked construction-state reference from one traced Value edge.
    pub(crate) fn pending_typed_array_construction_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingTypedArrayConstruction>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_typed_array_construction)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies construction state without retaining a no-GC borrow across callbacks.
    fn typed_array_construction_snapshot(
        &mut self,
        state: GcRef<PendingTypedArrayConstruction>,
    ) -> Result<TypedArrayConstructionSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_typed_array_construction)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(TypedArrayConstructionSnapshot {
                    source: pending.source,
                    target: pending.target,
                    new_target: pending.new_target,
                    prototype: pending.prototype,
                    byte_offset_argument: pending.byte_offset_argument,
                    length_argument: pending.length_argument,
                    kind: pending.kind,
                    mode: pending.mode,
                    byte_offset: pending.byte_offset,
                    index: pending.index,
                    length: pending.length,
                })
            })
        })
    }

    /// Publishes state in the destination register before any allocation-capable operation.
    #[inline]
    fn root_typed_array_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingTypedArrayConstruction>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Updates scalar construction state without requiring a generational barrier.
    fn update_typed_array_construction(
        &mut self,
        state: GcRef<PendingTypedArrayConstruction>,
        update: impl FnOnce(&mut PendingTypedArrayConstruction),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_typed_array_construction)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Replaces one traced state edge and records the old-to-young write barrier.
    fn set_typed_array_construction_value(
        &mut self,
        state: GcRef<PendingTypedArrayConstruction>,
        field: impl FnOnce(&mut PendingTypedArrayConstruction) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_typed_array_construction)
                    .map_err(ExecutionError::NoGcBorrow)?;
                *field(pending) = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Returns one shared `%TypedArray.prototype%` accessor result after a brand check.
    pub(crate) fn typed_array_getter(
        &mut self,
        receiver: Value,
        getter: TypedArrayGetter,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.typed_array_snapshot(receiver)?;
        let attached = match getter {
            TypedArrayGetter::Length
            | TypedArrayGetter::ByteLength
            | TypedArrayGetter::ByteOffset => match self.typed_array_backing(snapshot.buffer) {
                Ok(_) => true,
                Err(ExecutionError::DetachedArrayBuffer) => false,
                Err(error) => return Err(error),
            },
            TypedArrayGetter::Buffer | TypedArrayGetter::ToStringTag => true,
        };
        Ok(match getter {
            TypedArrayGetter::Length if !attached => Value::from_i32(0),
            TypedArrayGetter::Length => safe_integer_value(snapshot.length as u64),
            TypedArrayGetter::Buffer => snapshot.buffer,
            TypedArrayGetter::ByteLength | TypedArrayGetter::ByteOffset if !attached => {
                Value::from_i32(0)
            }
            TypedArrayGetter::ByteLength => safe_integer_value(
                snapshot
                    .length
                    .checked_mul(snapshot.kind.byte_width())
                    .ok_or(ExecutionError::InvalidArrayLength)? as u64,
            ),
            TypedArrayGetter::ByteOffset => safe_integer_value(snapshot.byte_offset as u64),
            TypedArrayGetter::ToStringTag => {
                let atom = self.intern_intrinsic_name(snapshot.kind.name().as_bytes())?;
                self.atom_string_value(atom)?
            }
        })
    }

    /// Classifies CanonicalNumericIndexString while keeping common array indices allocation-free.
    pub(crate) fn typed_array_index(
        &mut self,
        key: PropertyKey,
    ) -> Result<TypedArrayIndex, ExecutionError> {
        let Some(atom) = key.atom() else {
            return Ok(TypedArrayIndex::NonNumeric);
        };
        let string = self
            .atoms
            .get(atom)
            .ok_or(ExecutionError::InvalidAtom(atom))?;
        if let Some(index) = array_index(string.as_view()) {
            return Ok(TypedArrayIndex::Valid(index as usize));
        }
        let mut original = Vec::new();
        original
            .try_reserve_exact(string.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let view = string.as_view();
        original.extend(
            (0..view.len()).map(|index| view.code_unit_at(index).expect("index is bounded")),
        );
        if original == [u16::from(b'-'), u16::from(b'0')] {
            return Ok(TypedArrayIndex::Invalid);
        }
        let number = parse_number_code_units(&original);
        let mut canonical = Vec::new();
        self.append_primitive_string_units(Value::from_f64(number), &mut canonical)?;
        if canonical != original {
            return Ok(TypedArrayIndex::NonNumeric);
        }
        if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
            return Ok(TypedArrayIndex::Invalid);
        }
        usize::try_from(number as u64)
            .map(TypedArrayIndex::Valid)
            .map_err(|_| ExecutionError::InvalidArrayLength)
    }

    /// Reads one canonical numeric property, returning None only for non-numeric keys.
    pub(crate) fn typed_array_index_get(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Option<Value>>, ExecutionError> {
        if !self.is_typed_array_value(receiver) {
            return Ok(None);
        }
        let index = match self.typed_array_index(key)? {
            TypedArrayIndex::NonNumeric => return Ok(None),
            TypedArrayIndex::Invalid => return Ok(Some(None)),
            TypedArrayIndex::Valid(index) => index,
        };
        let snapshot = self.typed_array_snapshot(receiver)?;
        if index >= snapshot.length {
            return Ok(Some(None));
        }
        match self.typed_array_read_element(snapshot, index) {
            Ok(value) => Ok(Some(Some(value))),
            Err(ExecutionError::DetachedArrayBuffer) => Ok(Some(None)),
            Err(error) => Err(error),
        }
    }

    /// Writes one canonical numeric property, returning None only for non-numeric keys.
    pub(crate) fn typed_array_index_set(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<Option<bool>, ExecutionError> {
        if !self.is_typed_array_value(receiver) {
            return Ok(None);
        }
        let index = match self.typed_array_index(key)? {
            TypedArrayIndex::NonNumeric => return Ok(None),
            TypedArrayIndex::Invalid => return Ok(Some(false)),
            TypedArrayIndex::Valid(index) => index,
        };
        let snapshot = self.typed_array_snapshot(receiver)?;
        if index >= snapshot.length {
            return Ok(Some(false));
        }
        match self.typed_array_write_value(snapshot, index, value) {
            Ok(()) => Ok(Some(true)),
            Err(ExecutionError::DetachedArrayBuffer) => Ok(Some(false)),
            Err(error) => Err(error),
        }
    }

    #[inline(always)]
    pub(crate) fn is_typed_array_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.typed_array_object)
                .is_ok()
        })
    }

    /// Snapshots immutable fixed-view metadata without retaining a no-GC borrow.
    pub(crate) fn typed_array_snapshot(
        &mut self,
        value: Value,
    ) -> Result<TypedArraySnapshot, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let array = self
            .heap
            .checked_reference(raw, self.types.typed_array_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        let snapshot = self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(array, self.types.typed_array_object)
                    .map(|array| {
                        (
                            TypedArraySnapshot {
                                buffer: array.buffer,
                                byte_offset: array.byte_offset as usize,
                                length: array.length as usize,
                                kind: array.kind,
                            },
                            array.length_mode,
                        )
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let (snapshot, length_mode) = snapshot;
        let available = match self.array_buffer_length_for_view_value(snapshot.buffer) {
            Ok(length) => length,
            Err(ExecutionError::DetachedArrayBuffer) => 0,
            Err(error) => return Err(error),
        };
        let effective_length = if length_mode == ViewLengthMode::Tracking {
            available.saturating_sub(snapshot.byte_offset) / snapshot.kind.byte_width()
        } else {
            snapshot.length
        };
        let required = snapshot
            .byte_offset
            .checked_add(effective_length.saturating_mul(snapshot.kind.byte_width()))
            .ok_or(ExecutionError::InvalidArrayLength)?;
        if required > available {
            return Ok(TypedArraySnapshot {
                length: 0,
                ..snapshot
            });
        }
        Ok(TypedArraySnapshot {
            length: effective_length,
            ..snapshot
        })
    }

    /// Publishes the length-tracking mode after a newly allocated view is fully rooted.
    fn set_typed_array_length_mode(
        &mut self,
        value: Value,
        mode: ViewLengthMode,
    ) -> Result<(), ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let array = self
            .heap
            .checked_reference(raw, self.types.typed_array_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(array, self.types.typed_array_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .length_mode = mode;
                Ok(())
            })
        })
    }

    /// Returns whether the branded ArrayBuffer permits views to track growth.
    pub(crate) fn array_buffer_is_resizable(
        &mut self,
        buffer: Value,
    ) -> Result<bool, ExecutionError> {
        let raw = buffer
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(buffer))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(buffer))?;
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            let data = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data
                    .ok_or(ExecutionError::DetachedArrayBuffer)
            })?;
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map(|data| data.resizable)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Reads the current byte length through the ArrayBuffer edge for resize-aware views.
    fn array_buffer_length_for_view_value(
        &mut self,
        buffer: Value,
    ) -> Result<usize, ExecutionError> {
        let raw = buffer
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(buffer))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(buffer))?;
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            let data = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data
                    .ok_or(ExecutionError::DetachedArrayBuffer)
            })?;
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map(|data| data.byte_length)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Returns the current byte length while rejecting a detached ArrayBuffer edge.
    fn array_buffer_length_for_view(
        &mut self,
        buffer: GcRef<crate::object::ArrayBufferObject>,
    ) -> Result<usize, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let buffer = scope.root(buffer).map_err(ExecutionError::Root)?;
            let data = scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(buffer, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data
                    .ok_or(ExecutionError::DetachedArrayBuffer)
            })?;
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map(|data| data.byte_length)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Resolves the ArrayBuffer edge to the currently attached byte backing.
    fn typed_array_backing(
        &mut self,
        buffer: Value,
    ) -> Result<GcRef<ArrayBufferData>, ExecutionError> {
        let raw = buffer
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(buffer))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(buffer))?;
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data
                    .ok_or(ExecutionError::DetachedArrayBuffer)
            })
        })
    }

    /// Copies one checked element into a stack word and decodes explicit little-endian storage.
    pub(crate) fn typed_array_read_element(
        &mut self,
        array: TypedArraySnapshot,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        let width = array.kind.byte_width();
        let start = array
            .byte_offset
            .checked_add(
                index
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = start
            .checked_add(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let data = self.typed_array_backing(array.buffer)?;
        let bytes = self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut bytes = [0_u8; 8];
                bytes[..width].copy_from_slice(&data.bytes[start..end]);
                Ok(bytes)
            })
        })?;
        match array.kind.content_type() {
            ContentType::Number => Ok(data_view_decode(data_view_kind(array.kind)?, bytes, true)),
            ContentType::BigInt => self.allocate_bigint_bits(
                u64::from_le_bytes(bytes),
                array.kind == TypedArrayKind::BigInt64,
            ),
        }
    }

    /// Converts according to the target content type before committing one element.
    fn typed_array_write_value(
        &mut self,
        array: TypedArraySnapshot,
        index: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match array.kind.content_type() {
            ContentType::Number => {
                if self.is_bigint_value(value) {
                    return Err(ExecutionError::TypedArrayContentTypeMismatch);
                }
                let number = numeric_value(self.convert_to_number(value)?)
                    .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
                self.typed_array_write_element(array, index, number)
            }
            ContentType::BigInt => {
                let bigint = self.primitive_to_bigint(value)?;
                let bits = self.bigint_modulo_u64(bigint)?;
                self.typed_array_write_bigint_element(array, index, bits)
            }
        }
    }

    /// Normalizes one strict/SameValueZero search value without per-element BigInt allocation.
    fn typed_array_search_needle(
        &mut self,
        kind: TypedArrayKind,
        value: Value,
    ) -> Result<Option<TypedArraySearchNeedle>, ExecutionError> {
        match kind.content_type() {
            ContentType::Number => Ok(numeric_value(value).map(TypedArraySearchNeedle::Number)),
            ContentType::BigInt => {
                if !self.is_bigint_value(value) {
                    return Ok(None);
                }
                let bits = self.bigint_modulo_u64(value)?;
                let canonical =
                    self.allocate_bigint_bits(bits, kind == TypedArrayKind::BigInt64)?;
                self.bigint_equal(value, canonical)
                    .map(|equal| equal.then_some(TypedArraySearchNeedle::BigInt(bits)))
            }
        }
    }

    /// Encodes and writes one checked element without publishing a per-index property slot.
    fn typed_array_write_element(
        &mut self,
        array: TypedArraySnapshot,
        index: usize,
        number: f64,
    ) -> Result<(), ExecutionError> {
        let width = array.kind.byte_width();
        let start = array
            .byte_offset
            .checked_add(
                index
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = start
            .checked_add(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let bytes = if array.kind == TypedArrayKind::Uint8Clamped {
            let mut bytes = [0_u8; 8];
            bytes[0] = to_uint8_clamp(number);
            bytes
        } else {
            data_view_encode(data_view_kind(array.kind)?, number, true)
        };
        let data = self.typed_array_backing(array.buffer)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .bytes[start..end]
                    .copy_from_slice(&bytes[..width]);
                Ok(())
            })
        })
    }

    /// Stores one modulo-2^64 BigInt encoding as explicit little-endian two's complement bytes.
    fn typed_array_write_bigint_element(
        &mut self,
        array: TypedArraySnapshot,
        index: usize,
        bits: u64,
    ) -> Result<(), ExecutionError> {
        let start = array
            .byte_offset
            .checked_add(
                index
                    .checked_mul(8)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = start
            .checked_add(8)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let data = self.typed_array_backing(array.buffer)?;
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .bytes[start..end]
                    .copy_from_slice(&bits.to_le_bytes());
                Ok(())
            })
        })
    }
}

/// Applies ToLength to one already numeric array-like length value.
fn typed_array_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if number == f64::INFINITY {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor().min(MAX_SAFE_INTEGER as f64) as u64)
}

#[inline(always)]
fn data_view_kind(kind: TypedArrayKind) -> Result<DataViewElement, ExecutionError> {
    Ok(match kind {
        TypedArrayKind::Int8 => DataViewElement::Int8,
        TypedArrayKind::Uint8 | TypedArrayKind::Uint8Clamped => DataViewElement::Uint8,
        TypedArrayKind::Int16 => DataViewElement::Int16,
        TypedArrayKind::Uint16 => DataViewElement::Uint16,
        TypedArrayKind::Int32 => DataViewElement::Int32,
        TypedArrayKind::Uint32 => DataViewElement::Uint32,
        TypedArrayKind::Float32 => DataViewElement::Float32,
        TypedArrayKind::Float64 => DataViewElement::Float64,
        TypedArrayKind::BigInt64 | TypedArrayKind::BigUint64 => {
            return Err(ExecutionError::TypedArrayContentTypeMismatch);
        }
    })
}

/// Implements ToUint8Clamp including ties-to-even at exact half integers.
#[inline]
fn to_uint8_clamp(number: f64) -> u8 {
    if number.is_nan() || number <= 0.0 {
        return 0;
    }
    if number >= 255.0 {
        return 255;
    }
    let floor = number.floor();
    let fraction = number - floor;
    if fraction > 0.5 || (fraction == 0.5 && floor as u8 % 2 == 1) {
        floor as u8 + 1
    } else {
        floor as u8
    }
}
