//! Fixed-length ArrayBuffer construction and branded prototype accessors.

use super::super::*;
use crate::object::{ArrayBufferData, ArrayBufferObject};
use crate::runtime::fiber::ArrayBufferSliceStage;
use crate::tuning::buffers::ARRAY_BUFFER_SLICE_COPY_CHUNK_BYTES;

const MAX_ARRAY_BUFFER_BYTES: usize = u32::MAX as usize;

#[derive(Clone, Copy, Debug)]
struct ArrayBufferDataSnapshot {
    data: GcRef<ArrayBufferData>,
    byte_length: usize,
    max_byte_length: usize,
    resizable: bool,
}

struct ArrayBufferAllocationRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    data: Option<GcRef<ArrayBufferData>>,
}

impl Trace for ArrayBufferAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.data.trace(tracer);
    }
}

impl Isolate {
    /// Clears one fixed ArrayBuffer's backing edge; repeated detach is a no-op.
    pub(crate) fn detach_array_buffer(&mut self, value: Value) -> Result<(), ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(object, self.types.array_buffer_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .data = None;
                Ok(())
            })
        })
    }

    /// Implements the fixed-length `ArrayBuffer` constructor without host allocation hooks.
    pub(crate) fn create_array_buffer_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let length = self.call_argument(site, 0)?.unwrap_or(Value::from_i32(0));
        let length = numeric_value(self.convert_to_number(length)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(length))?;
        if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let length =
            usize::try_from(length as u64).map_err(|_| ExecutionError::InvalidArrayLength)?;
        if length > MAX_ARRAY_BUFFER_BYTES {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let mut max_byte_length = length;
        let mut resizable = false;
        let options = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(options) {
            let max_atom = self.intern_intrinsic_name(b"maxByteLength")?;
            if let Some(value) = self.get_data_property(options, max_atom)?
                && value.as_immediate() != Some(Immediate::Undefined)
            {
                max_byte_length = self.ecma_to_index(value)?;
                if max_byte_length < length {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                resizable = true;
            }
        }
        let prototype = self.array_buffer_prototype_for_new_target(site.new_target)?;
        self.allocate_array_buffer_object(length, max_byte_length, resizable, prototype)
    }

    /// Resizes a resizable ArrayBuffer in place while preserving its backing identity.
    pub(crate) fn resize_array_buffer(
        &mut self,
        receiver: Value,
        new_length: Value,
    ) -> Result<Value, ExecutionError> {
        let target = self.ecma_to_index(new_length)?;
        if target > MAX_ARRAY_BUFFER_BYTES {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let snapshot = self
            .array_buffer_data_snapshot(receiver)?
            .ok_or(ExecutionError::DetachedArrayBuffer)?;
        if !snapshot.resizable || target > snapshot.max_byte_length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        self.heap.with_running_scope(|scope| {
            let data = scope.root(snapshot.data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                data.byte_length = target;
                Ok::<(), ExecutionError>(())
            })
        })?;
        Ok(Value::from_immediate(Immediate::Undefined))
    }

    /// Selects a constructor prototype while preserving the ordinary cross-realm fallback.
    fn array_buffer_prototype_for_new_target(
        &mut self,
        new_target: Value,
    ) -> Result<Value, ExecutionError> {
        let current_fallback = self
            .realm
            .array_buffer_prototype
            .expect("ArrayBuffer prototype initializes before construction");
        if new_target.as_immediate() == Some(Immediate::Undefined) {
            return Ok(current_fallback);
        }
        let fallback = self
            .realm_for_callable(new_target)
            .ok()
            .and_then(|realm| {
                if realm == self.active_realm {
                    self.realm.array_buffer_prototype
                } else {
                    self.inactive_realms
                        .iter()
                        .find(|(id, _)| *id == realm)
                        .and_then(|(_, realm)| realm.array_buffer_prototype)
                }
            })
            .unwrap_or(current_fallback);
        let prototype_atom = self.intern_intrinsic_name(b"prototype")?;
        let prototype = self
            .constructor_prototype_value(new_target, prototype_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(prototype) {
            Ok(prototype)
        } else {
            Ok(fallback)
        }
    }

    /// Allocates a byte backing block and an ordinary branded ArrayBuffer object.
    pub(crate) fn allocate_array_buffer_object(
        &mut self,
        byte_length: usize,
        max_byte_length: usize,
        resizable: bool,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(max_byte_length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        bytes.resize(max_byte_length, 0);
        let mut roots = ArrayBufferAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            prototype,
            data: None,
        };
        let data = self
            .heap
            .try_allocate_external_with_gc(
                self.types.array_buffer_data,
                0,
                ArrayBufferData {
                    bytes: bytes.into_boxed_slice(),
                    byte_length,
                    max_byte_length,
                    resizable,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.data = Some(data);
        self.heap
            .try_allocate_with_gc(
                self.types.array_buffer_object,
                0,
                0,
                ArrayBufferObject {
                    data: Some(data),
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns a branded backing snapshot for a fixed ArrayBuffer operation.
    fn array_buffer_data_snapshot(
        &mut self,
        value: Value,
    ) -> Result<Option<ArrayBufferDataSnapshot>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let object = self
            .heap
            .checked_reference(raw, self.types.array_buffer_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        let data = self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.array_buffer_object)
                    .map(|object| object.data)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let Some(data) = data else { return Ok(None) };
        let snapshot = self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map(|data| (data.byte_length, data.max_byte_length, data.resizable))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok(Some(ArrayBufferDataSnapshot {
            data,
            byte_length: snapshot.0,
            max_byte_length: snapshot.1,
            resizable: snapshot.2,
        }))
    }

    /// Implements the branded `byteLength`, `maxByteLength`, `resizable`, and `detached` getters.
    pub(crate) fn array_buffer_getter(
        &mut self,
        receiver: Value,
        getter: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let Some(snapshot) = self.array_buffer_data_snapshot(receiver)? else {
            return Ok(match getter {
                NativeFunction::ArrayBufferDetached => Value::from_immediate(Immediate::True),
                NativeFunction::ArrayBufferByteLength
                | NativeFunction::ArrayBufferMaxByteLength => Value::from_i32(0),
                NativeFunction::ArrayBufferResizable => Value::from_immediate(Immediate::False),
                _ => return Err(ExecutionError::MissingNativeContinuation),
            });
        };
        Ok(match getter {
            NativeFunction::ArrayBufferByteLength => Value::from_f64(snapshot.byte_length as f64),
            NativeFunction::ArrayBufferMaxByteLength => {
                Value::from_f64(snapshot.max_byte_length as f64)
            }
            NativeFunction::ArrayBufferResizable => boolean_value(snapshot.resizable),
            NativeFunction::ArrayBufferDetached => Value::from_immediate(Immediate::False),
            _ => return Err(ExecutionError::MissingNativeContinuation),
        })
    }

    /// Implements `ArrayBuffer.isView` for the currently installed view brands.
    pub(crate) fn array_buffer_is_view(&mut self, value: Value) -> Value {
        let is_view = value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.data_view_object)
                .is_ok()
                || self
                    .heap
                    .checked_reference(raw, self.types.typed_array_object)
                    .is_ok()
        });
        boolean_value(is_view)
    }

    /// Begins the fixed, non-shared `ArrayBuffer.prototype.slice` algorithm.
    pub(crate) fn begin_array_buffer_slice(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let source = site.this_value;
        let snapshot = self
            .array_buffer_data_snapshot(source)?
            .ok_or(ExecutionError::DetachedArrayBuffer)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let start = self.call_argument(site, 0)?.unwrap_or(undefined);
        let end = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_array_buffer_slice_state(NativeCallState {
            values: [
                source,
                start,
                end,
                Value::from_f64(snapshot.byte_length as f64),
                undefined,
            ],
            count: 0,
        })?;
        let site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_buffer_slice_state(site, state)?;
        if self.is_object_value(start) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayBufferSliceStart,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                start,
                site.call_site,
            );
        }
        self.finish_array_buffer_slice_start(site, state, start)
    }

    /// Begins fixed ArrayBuffer copy-and-detach while preserving observable ToIndex order.
    pub(crate) fn begin_array_buffer_transfer(
        &mut self,
        site: &CallSite,
        to_fixed_length: bool,
    ) -> Result<(), ExecutionError> {
        let source = site.this_value;
        let initial = self.array_buffer_data_snapshot(source)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let new_length = self.call_argument(site, 0)?.unwrap_or(undefined);
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if new_length.as_immediate() == Some(Immediate::Undefined) {
            let length = initial
                .ok_or(ExecutionError::DetachedArrayBuffer)?
                .byte_length;
            return self.finish_array_buffer_transfer(native_site, source, length, to_fixed_length);
        }
        if self.is_object_value(new_length) {
            let state = self.allocate_array_buffer_slice_state(NativeCallState {
                values: [source, undefined, undefined, undefined, undefined],
                count: 0,
            })?;
            self.root_array_buffer_slice_state(native_site, state)?;
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayBufferTransferLength(to_fixed_length),
                native_site.caller_base,
                native_site.destination,
                Value::from_heap_ref(state.raw()),
                new_length,
                native_site.call_site,
            );
        }
        let new_length = self.ecma_to_index(new_length)?;
        self.finish_array_buffer_transfer(native_site, source, new_length, to_fixed_length)
    }

    /// Resumes explicit newLength conversion, then revalidates source attachment.
    pub(crate) fn resume_array_buffer_transfer_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        to_fixed_length: bool,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_buffer_slice_state(site, state)?;
        let source = self.native_call_state_snapshot(state)?.values[0];
        let new_length = self.ecma_to_index(value)?;
        self.finish_array_buffer_transfer(site, source, new_length, to_fixed_length)
    }

    /// Allocates and copies the complete result before atomically clearing the source edge.
    fn finish_array_buffer_transfer(
        &mut self,
        site: NativeContinuationSite,
        source: Value,
        new_length: usize,
        _to_fixed_length: bool,
    ) -> Result<(), ExecutionError> {
        if new_length > MAX_ARRAY_BUFFER_BYTES {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let source_snapshot = self
            .array_buffer_data_snapshot(source)?
            .ok_or(ExecutionError::DetachedArrayBuffer)?;
        let prototype = self
            .realm
            .array_buffer_prototype
            .expect("ArrayBuffer prototype initializes before transfer");
        let result = self.allocate_array_buffer_object(new_length, new_length, false, prototype)?;
        let result_snapshot = self
            .array_buffer_data_snapshot(result)?
            .expect("new ArrayBuffer result is attached");
        self.copy_array_buffer_slice_bytes(
            source_snapshot.data,
            result_snapshot.data,
            0,
            source_snapshot.byte_length.min(new_length),
        )?;
        self.detach_array_buffer(source)?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Resumes either observable index conversion without retaining Rust stack state.
    pub(crate) fn resume_array_buffer_slice_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_buffer_slice_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayBufferSliceStart => {
                self.finish_array_buffer_slice_start(site, state, value)
            }
            ConversionConsumer::ArrayBufferSliceEnd => {
                self.finish_array_buffer_slice_end(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Resumes constructor/species property reads and the custom construction result.
    pub(crate) fn resume_array_buffer_slice(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ArrayBufferSliceStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_buffer_slice_state(site, state)?;
        match stage {
            ArrayBufferSliceStage::Constructor => {
                self.resume_array_buffer_slice_constructor(site, state, value)
            }
            ArrayBufferSliceStage::Value => {
                self.finish_array_buffer_slice_species(site, state, value, true)
            }
            ArrayBufferSliceStage::Construct => {
                self.finish_array_buffer_slice_construct(site, state, value)
            }
        }
    }

    /// Normalizes start against the initial byte-length snapshot, then observes end.
    fn finish_array_buffer_slice_start(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = numeric_value(pending.values[3])
            .expect("ArrayBuffer slice stores its initial byte length as Number")
            as u64;
        let first = relative_array_buffer_slice_index(
            length,
            array_buffer_slice_integer(self.convert_to_number(value)?)?,
        );
        self.update_array_buffer_slice_value(state, 1, safe_integer_value(first))?;
        let end = pending.values[2];
        if end.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_array_buffer_slice_indices(site, state, length);
        }
        if self.is_object_value(end) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayBufferSliceEnd,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                end,
                site.call_site,
            );
        }
        self.finish_array_buffer_slice_end(site, state, end)
    }

    /// Normalizes the explicit end argument and freezes the resulting byte count.
    fn finish_array_buffer_slice_end(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let length = numeric_value(pending.values[3])
            .expect("ArrayBuffer slice stores its initial byte length as Number")
            as u64;
        let final_index = relative_array_buffer_slice_index(
            length,
            array_buffer_slice_integer(self.convert_to_number(value)?)?,
        );
        self.finish_array_buffer_slice_indices(site, state, final_index)
    }

    /// Saves `newLen`, then starts the observable SpeciesConstructor lookup.
    fn finish_array_buffer_slice_indices(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        final_index: u64,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let first = numeric_value(pending.values[1])
            .expect("ArrayBuffer slice stores first as Number") as u64;
        self.update_array_buffer_slice_value(
            state,
            2,
            safe_integer_value(final_index.saturating_sub(first)),
        )?;
        let constructor = self.constructor_atom()?;
        if let Some(value) = self.dispatch_array_buffer_slice_get(
            site,
            state,
            ArrayBufferSliceStage::Constructor,
            pending.values[0],
            constructor.into(),
        )? {
            self.resume_array_buffer_slice_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Reads `@@species` when constructor is an object, otherwise applies SpeciesConstructor rules.
    fn resume_array_buffer_slice_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_array_buffer_slice_species(site, state, constructor, false);
        }
        if !self.is_object_value(constructor) {
            return Err(ExecutionError::NotObject(constructor));
        }
        let constructor_realm = if self.is_constructor_value(constructor)? {
            let constructor_realm = self.realm_for_callable(constructor)?;
            Some(constructor_realm)
        } else {
            None
        };
        self.update_array_buffer_slice_value(state, 3, constructor)?;
        let species = constructor_realm
            .and_then(|realm| {
                if realm == self.active_realm {
                    self.realm.well_known_symbols.species
                } else {
                    self.inactive_realms
                        .iter()
                        .find(|(id, _)| *id == realm)
                        .and_then(|(_, realm)| realm.well_known_symbols.species)
                }
            })
            .or(self.realm.well_known_symbols.species)
            .expect("Symbol.species initializes before ArrayBuffer");
        let species_key = self.property_key(species)?;
        if let Some(value) = self.dispatch_array_buffer_slice_get(
            site,
            state,
            ArrayBufferSliceStage::Value,
            constructor,
            species_key,
        )? {
            self.finish_array_buffer_slice_species(site, state, value, true)?;
        }
        Ok(())
    }

    /// Selects the intrinsic constructor for undefined/null species and starts Construct.
    fn finish_array_buffer_slice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        observed: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        let constructor = if observed.as_immediate() == Some(Immediate::Undefined)
            || (from_species && observed.as_immediate() == Some(Immediate::Null))
        {
            self.realm
                .array_buffer_constructor
                .expect("ArrayBuffer constructor initializes before slice")
        } else {
            observed
        };
        if !self.is_constructor_value(constructor)? {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_array_buffer_slice_result(site, state, constructor)
    }

    /// Roots the one exact length argument while the selected constructor executes.
    fn construct_array_buffer_slice_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.update_array_buffer_slice_value(state, 3, constructor)?;
        let new_length = self.native_call_state_snapshot(state)?.values[2];
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        arguments.push(new_length);
        self.push_array_buffer_slice_parent(
            site,
            state,
            ArrayBufferSliceStage::Construct,
            constructor,
        )?;
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, arguments) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_array_buffer_slice_parent(
            site,
            state,
            ArrayBufferSliceStage::Construct,
            Value::from_heap_ref(prefix.raw()),
        )?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.construct_site(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: constructor,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 1,
            argument_count: 1,
            this_value: undefined,
            new_target: constructor,
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("ArrayBuffer species constructor publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_buffer_slice_construct(site, state, result)
    }

    /// Validates the constructor result and source ordering before copying bytes.
    fn finish_array_buffer_slice_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let result_snapshot = self
            .array_buffer_data_snapshot(result)?
            .ok_or(ExecutionError::DetachedArrayBuffer)?;
        let pending = self.native_call_state_snapshot(state)?;
        let source = pending.values[0];
        if result == source {
            return Err(ExecutionError::NotObject(result));
        }
        let new_length = numeric_value(pending.values[2])
            .expect("ArrayBuffer slice stores new length as Number")
            as usize;
        if result_snapshot.byte_length < new_length {
            return Err(ExecutionError::NotObject(result));
        }
        let source_snapshot = self
            .array_buffer_data_snapshot(source)?
            .ok_or(ExecutionError::DetachedArrayBuffer)?;
        self.update_array_buffer_slice_value(state, 4, result)?;
        let first = numeric_value(pending.values[1])
            .expect("ArrayBuffer slice stores first as Number") as usize;
        self.copy_array_buffer_slice_bytes(
            source_snapshot.data,
            result_snapshot.data,
            first,
            new_length,
        )?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Copies bytes through bounded stack scratch so no untraced allocation or aliased borrow exists.
    fn copy_array_buffer_slice_bytes(
        &mut self,
        source: GcRef<ArrayBufferData>,
        destination: GcRef<ArrayBufferData>,
        first: usize,
        length: usize,
    ) -> Result<(), ExecutionError> {
        let mut scratch = [0_u8; ARRAY_BUFFER_SLICE_COPY_CHUNK_BYTES];
        let mut copied = 0;
        while copied < length {
            let chunk = (length - copied).min(scratch.len());
            self.read_array_buffer_slice_chunk(source, first + copied, &mut scratch[..chunk])?;
            self.write_array_buffer_slice_chunk(destination, copied, &scratch[..chunk])?;
            copied += chunk;
        }
        Ok(())
    }

    /// Reads one checked source range under a no-GC borrow.
    fn read_array_buffer_slice_chunk(
        &mut self,
        data: GcRef<ArrayBufferData>,
        offset: usize,
        output: &mut [u8],
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let end = offset
                    .checked_add(output.len())
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                let bytes = data
                    .bytes
                    .get(offset..end)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                output.copy_from_slice(bytes);
                Ok(())
            })
        })
    }

    /// Writes one checked destination range under a no-GC borrow.
    fn write_array_buffer_slice_chunk(
        &mut self,
        data: GcRef<ArrayBufferData>,
        offset: usize,
        input: &[u8],
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow_mut(data, self.types.array_buffer_data)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let end = offset
                    .checked_add(input.len())
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                let bytes = data
                    .bytes
                    .get_mut(offset..end)
                    .ok_or(ExecutionError::InvalidArrayLength)?;
                bytes.copy_from_slice(input);
                Ok(())
            })
        })
    }

    /// Dispatches a Proxy/accessor-aware property read with the slice state rooted beneath it.
    fn dispatch_array_buffer_slice_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ArrayBufferSliceStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_buffer_slice_parent(site, state, stage, receiver)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(None);
        }
        self.pop_native_continuation()?;
        self.read(site.caller_base, site.destination).map(Some)
    }

    /// Pushes the typed parent continuation used by property and constructor callbacks.
    fn push_array_buffer_slice_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ArrayBufferSliceStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_buffer_slice(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Allocates one existing fixed five-Value native state under all VM roots.
    fn allocate_array_buffer_slice_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Keeps the state live in the caller destination across every allocation point.
    #[inline]
    fn root_array_buffer_slice_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Updates one traced state slot and applies the old-to-young barrier.
    fn update_array_buffer_slice_value(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}

#[inline(always)]
fn array_buffer_slice_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else if number.is_finite() {
        number.trunc()
    } else {
        number
    })
}

#[inline(always)]
fn relative_array_buffer_slice_index(length: u64, relative: f64) -> u64 {
    if relative <= -(length as f64) {
        0
    } else if relative < 0.0 {
        (length as f64 + relative) as u64
    } else if relative >= length as f64 {
        length
    } else {
        relative as u64
    }
}
