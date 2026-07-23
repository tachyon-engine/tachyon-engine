//! Resumable change-array-by-copy algorithms with dense result semantics.

use super::*;

/// The copy traversal selected by the invoked Array prototype method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayCopyKind {
    ToReversed,
    With,
}

/// GC-owned inputs and cursor state across observable source operations.
#[derive(Debug)]
pub(crate) struct PendingArrayCopy {
    receiver: Value,
    result: Value,
    retained: Value,
    index_argument: Value,
    replacement: Value,
    kind: ArrayCopyKind,
    length: u64,
    cursor: u64,
    replacement_index: u64,
}

impl Trace for PendingArrayCopy {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.result.trace(tracer);
        self.retained.trace(tracer);
        self.index_argument.trace(tracer);
        self.replacement.trace(tracer);
    }
}

#[derive(Clone, Copy)]
struct ArrayCopySnapshot {
    receiver: Value,
    result: Value,
    replacement: Value,
    kind: ArrayCopyKind,
    length: u64,
    cursor: u64,
    replacement_index: u64,
}

impl Isolate {
    /// Captures method arguments before the observable receiver length lookup.
    pub(crate) fn begin_array_copy(
        &mut self,
        site: &CallSite,
        kind: ArrayCopyKind,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let index_argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let replacement = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_array_copy_state(PendingArrayCopy {
            receiver,
            result: undefined,
            retained: undefined,
            index_argument,
            replacement,
            kind,
            length: 0,
            cursor: 0,
            replacement_index: 0,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_copy_state(native_site, state)?;
        let length = self.length_atom()?;
        self.get_array_copy_property(
            native_site,
            state,
            ArrayCopyStage::Length,
            receiver,
            length.into(),
        )
    }

    /// Routes one observable completion back into the active copy algorithm.
    pub(crate) fn resume_array_copy(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        stage: ArrayCopyStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_copy_state(site, state)?;
        match stage {
            ArrayCopyStage::Length => self.resume_array_copy_length(site, state, value),
            ArrayCopyStage::SourceValue => self.finish_array_copy_source(site, state, value),
        }
    }

    /// Resumes a length or relative-index object-to-primitive conversion.
    pub(crate) fn resume_array_copy_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_copy_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayCopyLength => {
                self.finish_array_copy_length(site, state, value)
            }
            ConversionConsumer::ArrayCopyIndex => self.finish_array_copy_index(site, state, value),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts LengthOfArrayLike while allowing user primitive-conversion callbacks.
    fn resume_array_copy_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayCopyLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_copy_length(site, state, value)
    }

    /// Creates the intrinsic dense result after storing the normalized length.
    fn finish_array_copy_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = array_copy_to_length(self.convert_to_number(value)?)?;
        self.update_array_copy_scalars(state, |pending| pending.length = length)?;
        if self.array_copy_snapshot(state)?.kind == ArrayCopyKind::With {
            return self.begin_array_copy_index(site, state);
        }
        self.create_array_copy_result(site, state)
    }

    /// Allocates the intrinsic result only after every required argument conversion.
    fn create_array_copy_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
    ) -> Result<(), ExecutionError> {
        let length = self.array_copy_snapshot(state)?.length;
        if length > u64::from(u32::MAX) {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before copy methods");
        let result = self.create_array_object_with_prototype(prototype)?;
        let state =
            self.pending_array_copy_reference(self.read(site.caller_base, site.destination)?)?;
        self.set_array_copy_value(state, |pending| &mut pending.result, result)?;
        self.set_array_length_value(result, safe_integer_value(length))?;
        self.advance_array_copy(site, state)
    }

    /// Converts the `with` relative index only after the result Array is created.
    fn begin_array_copy_index(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
    ) -> Result<(), ExecutionError> {
        let index = self.array_copy_index_argument(state)?;
        if self.is_object_value(index) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayCopyIndex,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                index,
                site.call_site,
            );
        }
        self.finish_array_copy_index(site, state, index)
    }

    /// Resolves and validates the `with` index before any source element Get.
    fn finish_array_copy_index(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let relative = array_copy_integer(self.convert_to_number(value)?)?;
        let length = self.array_copy_snapshot(state)?.length;
        let Some(index) = relative_array_index(relative, length) else {
            return Err(ExecutionError::InvalidArrayLength);
        };
        self.update_array_copy_scalars(state, |pending| pending.replacement_index = index)?;
        self.create_array_copy_result(site, state)
    }

    /// Reads each required source index in the specified observable order.
    fn advance_array_copy(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_copy_snapshot(state)?;
        if snapshot.cursor >= snapshot.length {
            return self.write(site.caller_base, site.destination, snapshot.result);
        }
        if snapshot.kind == ArrayCopyKind::With && snapshot.cursor == snapshot.replacement_index {
            return self.define_array_copy_value(site, state, snapshot.replacement);
        }
        let source_index = match snapshot.kind {
            ArrayCopyKind::ToReversed => snapshot.length - snapshot.cursor - 1,
            ArrayCopyKind::With => snapshot.cursor,
        };
        let key = self.safe_integer_property_atom(source_index)?;
        self.get_array_copy_property(
            site,
            state,
            ArrayCopyStage::SourceValue,
            snapshot.receiver,
            key.into(),
        )
    }

    /// Defines the dense result property and advances only after success.
    fn finish_array_copy_source(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_copy_value(state, |pending| &mut pending.retained, value)?;
        self.define_array_copy_value(site, state, value)
    }

    /// Creates one ordinary data property on the fresh intrinsic result Array.
    fn define_array_copy_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_copy_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.cursor)?;
        self.define_data_property(
            snapshot.result,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            },
        )?;
        self.update_array_copy_scalars(state, |pending| pending.cursor += 1)?;
        self.advance_array_copy(site, state)
    }

    /// Publishes a typed parent around one Proxy/accessor-aware property Get.
    fn get_array_copy_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        stage: ArrayCopyStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_copy_parent(site, state, stage, receiver)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_copy_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_array_copy(site, state, stage, value)
    }

    /// Pushes one typed parent that owns state across nested JavaScript work.
    fn push_array_copy_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
        stage: ArrayCopyStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_copy(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots state in the destination register before any safepoint.
    #[inline]
    fn root_array_copy_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopy>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates one fixed-size copy state under the complete VM root set.
    fn allocate_array_copy_state(
        &mut self,
        pending: PendingArrayCopy,
    ) -> Result<GcRef<PendingArrayCopy>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_array_copy,
                0,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Validates and recovers one managed copy-state reference.
    pub(crate) fn pending_array_copy_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayCopy>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_copy)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies state fields without retaining a managed borrow across safepoints.
    fn array_copy_snapshot(
        &mut self,
        state: GcRef<PendingArrayCopy>,
    ) -> Result<ArrayCopySnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_copy)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayCopySnapshot {
                    receiver: pending.receiver,
                    result: pending.result,
                    replacement: pending.replacement,
                    kind: pending.kind,
                    length: pending.length,
                    cursor: pending.cursor,
                    replacement_index: pending.replacement_index,
                })
            })
        })
    }

    /// Reads the captured relative-index argument.
    fn array_copy_index_argument(
        &mut self,
        state: GcRef<PendingArrayCopy>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_array_copy)
                    .map(|pending| pending.index_argument)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Updates scalar state fields without requiring a write barrier.
    fn update_array_copy_scalars(
        &mut self,
        state: GcRef<PendingArrayCopy>,
        update: impl FnOnce(&mut PendingArrayCopy),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_copy)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates one traced state edge and records its generational barrier.
    fn set_array_copy_value(
        &mut self,
        state: GcRef<PendingArrayCopy>,
        field: impl FnOnce(&mut PendingArrayCopy) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_copy)
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
}

#[inline(always)]
fn array_copy_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}

#[inline(always)]
fn array_copy_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number == 0.0 {
        return Ok(0.0);
    }
    Ok(number.trunc())
}

#[inline(always)]
fn relative_array_index(relative: f64, length: u64) -> Option<u64> {
    if relative >= 0.0 {
        return (relative < length as f64).then_some(relative as u64);
    }
    if !relative.is_finite() || -relative > length as f64 {
        return None;
    }
    let index = length - (-relative as u64);
    (index < length).then_some(index)
}
