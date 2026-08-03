//! Safe SharedArrayBuffer backing and the non-waiting constructor/prototype surface.

use std::sync::{Arc, Mutex, MutexGuard};

use super::super::*;
use super::array_buffer::BufferSliceKind;
use crate::object::ArrayBufferObject;
use crate::runtime::fiber::SharedArrayBufferConstructorStage;

const MAX_SHARED_ARRAY_BUFFER_BYTES: usize = u32::MAX as usize;
const SHARED_CONSTRUCTOR_NEW_TARGET: usize = 0;
const SHARED_CONSTRUCTOR_LENGTH: usize = 1;
const SHARED_CONSTRUCTOR_OPTIONS: usize = 2;
const SHARED_CONSTRUCTOR_MAXIMUM: usize = 3;
const SHARED_CONSTRUCTOR_FALLBACK: usize = 4;

#[derive(Clone)]
struct SharedArrayBufferSnapshot {
    backing: Arc<Mutex<SharedArrayBufferBacking>>,
    byte_length: usize,
    max_byte_length: usize,
    growable: bool,
}

struct SharedArrayBufferAllocationRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    data: Option<GcRef<SharedArrayBufferData>>,
}

impl Trace for SharedArrayBufferAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.data.trace(tracer);
    }
}

impl Isolate {
    /// Begins observable SharedArrayBuffer length and option conversion under GC-managed state.
    pub(crate) fn begin_shared_array_buffer_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let length = self.call_argument(site, 0)?.unwrap_or(undefined);
        let options = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_array_buffer_slice_state(NativeCallState {
            values: [site.new_target, length, options, undefined, undefined],
            count: 0,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_buffer_slice_state(continuation_site, state)?;
        if self.is_object_value(length) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::SharedArrayBufferLength,
                continuation_site.caller_base,
                continuation_site.destination,
                Value::from_heap_ref(state.raw()),
                length,
                continuation_site.call_site,
            );
        }
        self.finish_shared_array_buffer_length(continuation_site, state, length)
    }

    /// Resumes either constructor ToIndex conversion without retaining Rust stack state.
    pub(crate) fn resume_shared_array_buffer_constructor_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_buffer_slice_state(site, state)?;
        match consumer {
            ConversionConsumer::SharedArrayBufferLength => {
                self.finish_shared_array_buffer_length(site, state, value)
            }
            ConversionConsumer::SharedArrayBufferMaxByteLength => {
                self.finish_shared_array_buffer_maximum(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Freezes the converted byte length, then reads and converts the optional maximum.
    fn finish_shared_array_buffer_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let byte_length = self.ecma_to_index(value)?;
        self.update_array_buffer_slice_value(
            state,
            SHARED_CONSTRUCTOR_LENGTH,
            safe_integer_value(byte_length as u64),
        )?;
        let options = self.native_call_state_snapshot(state)?.values[SHARED_CONSTRUCTOR_OPTIONS];
        if !self.is_object_value(options) {
            return self.finish_shared_array_buffer_allocation(site, state, byte_length, false);
        }
        let max_atom = self.intern_intrinsic_name(b"maxByteLength")?;
        let Some(maximum) = self.dispatch_shared_array_buffer_constructor_get(
            site,
            state,
            SharedArrayBufferConstructorStage::Maximum,
            options,
            max_atom.into(),
        )?
        else {
            return Ok(());
        };
        self.resume_shared_array_buffer_maximum(site, state, maximum)
    }

    /// Applies fixed semantics for undefined or converts an observed maximum value.
    fn resume_shared_array_buffer_maximum(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        maximum: Value,
    ) -> Result<(), ExecutionError> {
        let byte_length = numeric_value(
            self.native_call_state_snapshot(state)?.values[SHARED_CONSTRUCTOR_LENGTH],
        )
        .expect("SharedArrayBuffer state stores length as Number")
            as usize;
        if maximum.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_shared_array_buffer_allocation(site, state, byte_length, false);
        }
        if self.is_object_value(maximum) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::SharedArrayBufferMaxByteLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                maximum,
                site.call_site,
            );
        }
        self.finish_shared_array_buffer_maximum(site, state, maximum)
    }

    /// Validates the requested maximum before selecting the constructor prototype.
    fn finish_shared_array_buffer_maximum(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        maximum: Value,
    ) -> Result<(), ExecutionError> {
        let maximum = self.ecma_to_index(maximum)?;
        let byte_length = numeric_value(
            self.native_call_state_snapshot(state)?.values[SHARED_CONSTRUCTOR_LENGTH],
        )
        .expect("SharedArrayBuffer state stores length as Number")
            as usize;
        if maximum < byte_length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        self.finish_shared_array_buffer_allocation(site, state, maximum, true)
    }

    /// Observes `newTarget.prototype` before applying the engine backing-size ceiling.
    fn finish_shared_array_buffer_allocation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        max_byte_length: usize,
        growable: bool,
    ) -> Result<(), ExecutionError> {
        self.update_array_buffer_slice_value(
            state,
            SHARED_CONSTRUCTOR_MAXIMUM,
            safe_integer_value(max_byte_length as u64),
        )?;
        self.update_shared_array_buffer_constructor_count(state, u8::from(growable))?;
        let pending = self.native_call_state_snapshot(state)?;
        let fallback = self
            .shared_array_buffer_prototype_fallback(pending.values[SHARED_CONSTRUCTOR_NEW_TARGET]);
        self.update_array_buffer_slice_value(state, SHARED_CONSTRUCTOR_FALLBACK, fallback)?;
        let prototype_atom = self.intern_intrinsic_name(b"prototype")?;
        let Some(prototype) = self.dispatch_shared_array_buffer_constructor_get(
            site,
            state,
            SharedArrayBufferConstructorStage::Prototype,
            pending.values[SHARED_CONSTRUCTOR_NEW_TARGET],
            prototype_atom.into(),
        )?
        else {
            return Ok(());
        };
        self.finish_shared_array_buffer_prototype(site, state, prototype)
    }

    /// Chooses the observed prototype or fallback before allocating the shared byte block.
    fn finish_shared_array_buffer_prototype(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        observed: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let byte_length = numeric_value(pending.values[SHARED_CONSTRUCTOR_LENGTH])
            .expect("SharedArrayBuffer state stores length as Number")
            as usize;
        let max_byte_length = numeric_value(pending.values[SHARED_CONSTRUCTOR_MAXIMUM])
            .expect("SharedArrayBuffer state stores maximum as Number")
            as usize;
        let prototype = if self.is_object_value(observed) {
            observed
        } else {
            pending.values[SHARED_CONSTRUCTOR_FALLBACK]
        };
        if byte_length > MAX_SHARED_ARRAY_BUFFER_BYTES
            || max_byte_length > MAX_SHARED_ARRAY_BUFFER_BYTES
        {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let result = self.allocate_shared_array_buffer_object(
            byte_length,
            max_byte_length,
            pending.count != 0,
            prototype,
        )?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Computes the ordinary cross-realm fallback without observing `newTarget.prototype`.
    fn shared_array_buffer_prototype_fallback(&mut self, new_target: Value) -> Value {
        let current_fallback = self
            .realm
            .shared_array_buffer_prototype
            .expect("SharedArrayBuffer prototype initializes before construction");
        self.realm_for_callable(new_target)
            .ok()
            .and_then(|realm| {
                if realm == self.active_realm {
                    self.realm.shared_array_buffer_prototype
                } else {
                    self.inactive_realms
                        .iter()
                        .find(|(id, _)| *id == realm)
                        .and_then(|(_, realm)| realm.shared_array_buffer_prototype)
                }
            })
            .unwrap_or(current_fallback)
    }

    /// Resumes one observable constructor property read at its typed continuation boundary.
    pub(crate) fn resume_shared_array_buffer_constructor(
        &mut self,
        stage: SharedArrayBufferConstructorStage,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_buffer_slice_state(site, state)?;
        match stage {
            SharedArrayBufferConstructorStage::Maximum => {
                self.resume_shared_array_buffer_maximum(site, state, value)
            }
            SharedArrayBufferConstructorStage::Prototype => {
                self.finish_shared_array_buffer_prototype(site, state, value)
            }
        }
    }

    /// Dispatches a Proxy/accessor-aware constructor property read with state rooted beneath it.
    fn dispatch_shared_array_buffer_constructor_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: SharedArrayBufferConstructorStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::shared_array_buffer_constructor(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                receiver,
            ))
            .map_err(Isolate::completion_stack_error)?;
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

    /// Updates the compact growable flag stored beside constructor Values.
    fn update_shared_array_buffer_constructor_count(
        &mut self,
        state: GcRef<NativeCallState>,
        count: u8,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .count = count;
                Ok(())
            })
        })
    }

    /// Allocates one Arc-backed shared data block and its ordinary branded wrapper.
    pub(crate) fn allocate_shared_array_buffer_object(
        &mut self,
        byte_length: usize,
        max_byte_length: usize,
        growable: bool,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(max_byte_length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        bytes.resize(max_byte_length, 0);
        let backing = Arc::new(Mutex::new(SharedArrayBufferBacking {
            bytes: bytes.into_boxed_slice(),
            byte_length,
            max_byte_length,
            growable,
        }));
        let mut roots = SharedArrayBufferAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            prototype,
            data: None,
        };
        let data = self
            .heap
            .try_allocate_external_with_gc(
                self.types.shared_array_buffer_data,
                0,
                SharedArrayBufferData {
                    backing,
                    external_bytes: max_byte_length,
                },
                AllocationSpace::Old,
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
                    data: None,
                    shared_data: Some(data),
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                },
                AllocationSpace::Old,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Implements the three branded SharedArrayBuffer prototype accessors.
    pub(crate) fn shared_array_buffer_getter(
        &mut self,
        receiver: Value,
        getter: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.shared_array_buffer_snapshot(receiver)?;
        Ok(match getter {
            NativeFunction::SharedArrayBufferByteLength => {
                Value::from_f64(snapshot.byte_length as f64)
            }
            NativeFunction::SharedArrayBufferMaxByteLength => {
                Value::from_f64(snapshot.max_byte_length as f64)
            }
            NativeFunction::SharedArrayBufferGrowable => boolean_value(snapshot.growable),
            _ => return Err(ExecutionError::MissingNativeContinuation),
        })
    }

    /// Grows the visible prefix without reallocating or exposing a partial length transition.
    pub(crate) fn grow_shared_array_buffer(
        &mut self,
        receiver: Value,
        new_length: Value,
    ) -> Result<Value, ExecutionError> {
        let new_length = self.ecma_to_index(new_length)?;
        let snapshot = self.shared_array_buffer_snapshot(receiver)?;
        let mut backing = lock_shared_backing(&snapshot.backing);
        if !backing.growable {
            return Err(ExecutionError::FixedLengthSharedArrayBuffer);
        }
        if new_length < backing.byte_length || new_length > backing.max_byte_length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        backing.byte_length = new_length;
        Ok(Value::from_immediate(Immediate::Undefined))
    }

    /// Begins grow after brand validation, then suspends only for object primitive conversion.
    pub(crate) fn begin_shared_array_buffer_grow(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.shared_array_buffer_snapshot(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let new_length = self.call_argument(site, 0)?.unwrap_or(undefined);
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if !self.is_object_value(new_length) {
            return self.finish_shared_array_buffer_grow(
                continuation_site,
                site.this_value,
                new_length,
            );
        }
        let state = self.allocate_array_buffer_slice_state(NativeCallState {
            values: [site.this_value, undefined, undefined, undefined, undefined],
            count: 0,
        })?;
        self.root_array_buffer_slice_state(continuation_site, state)?;
        self.dispatch_object_primitive_conversion(
            ConversionConsumer::SharedArrayBufferGrowLength,
            continuation_site.caller_base,
            continuation_site.destination,
            Value::from_heap_ref(state.raw()),
            new_length,
            continuation_site.call_site,
        )
    }

    /// Resumes grow conversion and revalidates current shared backing state before mutation.
    pub(crate) fn resume_shared_array_buffer_grow_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_buffer_slice_state(site, state)?;
        let receiver = self.native_call_state_snapshot(state)?.values[0];
        self.finish_shared_array_buffer_grow(site, receiver, value)
    }

    /// Commits one converted grow length and writes undefined to the original destination.
    fn finish_shared_array_buffer_grow(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let result = self.grow_shared_array_buffer(receiver, value)?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Begins the shared-brand slice through the common resumable species algorithm.
    pub(crate) fn begin_shared_array_buffer_slice(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_buffer_slice(site, BufferSliceKind::Shared)
    }

    /// Resolves a SharedArrayBuffer wrapper and snapshots its mutex-protected scalar state.
    fn shared_array_buffer_snapshot(
        &mut self,
        value: Value,
    ) -> Result<SharedArrayBufferSnapshot, ExecutionError> {
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
                    .map(|object| object.shared_data)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let data = data.ok_or(ExecutionError::NotObject(value))?;
        let backing = self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.shared_array_buffer_data)
                    .map(|data| Arc::clone(&data.backing))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let locked = lock_shared_backing(&backing);
        let snapshot = SharedArrayBufferSnapshot {
            backing: Arc::clone(&backing),
            byte_length: locked.byte_length,
            max_byte_length: locked.max_byte_length,
            growable: locked.growable,
        };
        drop(locked);
        Ok(snapshot)
    }
}

/// Recovers a poisoned lock only for non-default unwind builds; production panic strategy aborts.
fn lock_shared_backing(
    backing: &Arc<Mutex<SharedArrayBufferBacking>>,
) -> MutexGuard<'_, SharedArrayBufferBacking> {
    backing
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
