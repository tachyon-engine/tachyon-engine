//! Resumable ES2025 Set composition and relation methods.

use super::*;

/// Shared policy selected by the seven Set methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum SetOperationKind {
    Union,
    Difference,
    Intersection,
    SymmetricDifference,
    IsSubsetOf,
    IsSupersetOf,
    IsDisjointFrom,
}

/// GC-owned state for `GetSetRecord`, callback scans, and arbitrary set-like iterators.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingSetOperation {
    receiver: Value,
    other: Value,
    has: Value,
    keys: Value,
    iterator: Value,
    next: Value,
    iterator_result: Value,
    result: Value,
    current: Value,
    kind: SetOperationKind,
    stage: SetOperationStage,
    cursor: u32,
    scan_limit: u32,
    other_size: u64,
}

impl Trace for PendingSetOperation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.other.trace(tracer);
        self.has.trace(tracer);
        self.keys.trace(tracer);
        self.iterator.trace(tracer);
        self.next.trace(tracer);
        self.iterator_result.trace(tracer);
        self.result.trace(tracer);
        self.current.trace(tracer);
    }
}

struct PendingSetOperationRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingSetOperation,
}

impl Trace for PendingSetOperationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Begins one Set method after validating the receiver's private Set slot.
    pub(crate) fn begin_set_operation(
        &mut self,
        site: &CallSite,
        kind: SetOperationKind,
    ) -> Result<(), ExecutionError> {
        self.set_storage(site.this_value)?;
        let other = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(other) {
            return Err(ExecutionError::NotObject(other));
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_pending_set_operation(PendingSetOperation {
            receiver: site.this_value,
            other,
            has: undefined,
            keys: undefined,
            iterator: undefined,
            next: undefined,
            iterator_result: undefined,
            result: undefined,
            current: undefined,
            kind,
            stage: SetOperationStage::Size,
            cursor: 0,
            scan_limit: 0,
            other_size: 0,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_set_operation(native_site, state)?;
        self.get_set_operation_property(native_site, state, SetOperationStage::Size, other, b"size")
    }

    /// Resumes one observable Get, Call, or iterator step of a shared Set operation.
    pub(crate) fn resume_set_operation(
        &mut self,
        continuation: NativeContinuation,
        stage: SetOperationStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.pending_set_operation_reference(continuation.first())?;
        self.root_set_operation(site, state)?;
        self.update_set_operation(state, |pending| pending.stage = stage)?;
        match stage {
            SetOperationStage::Size => self.resume_set_size(site, state, value),
            SetOperationStage::Has => {
                self.resolve_function_object(value)?;
                self.update_set_operation_value(state, |pending| &mut pending.has, value)?;
                let other = self.set_operation_snapshot(state)?.other;
                self.get_set_operation_property(
                    site,
                    state,
                    SetOperationStage::Keys,
                    other,
                    b"keys",
                )
            }
            SetOperationStage::Keys => {
                self.resolve_function_object(value)?;
                self.update_set_operation_value(state, |pending| &mut pending.keys, value)?;
                self.start_set_operation_after_record(site, state)
            }
            SetOperationStage::IteratorCall => self.resume_set_iterator_call(site, state, value),
            SetOperationStage::NextMethod => self.resume_set_next_method(site, state, value),
            SetOperationStage::NextCall => self.resume_set_next_call(site, state, value),
            SetOperationStage::ResultDone => self.resume_set_result_done(site, state, value),
            SetOperationStage::ResultValue => self.resume_set_result_value(site, state, value),
            SetOperationStage::HasCall => self.resume_set_has_result(site, state, value),
            SetOperationStage::CloseReturnGetter => {
                self.resume_set_close_getter(site, state, value)
            }
            SetOperationStage::CloseReturnCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.finish_set_boolean(site, false)
            }
        }
    }

    /// Resumes the object-to-primitive half of `GetSetRecord` size conversion.
    pub(crate) fn resume_set_size_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_set_operation(site, state)?;
        self.finish_set_size(site, state, value)
    }

    /// Converts raw size without holding an untraced object across ToPrimitive callbacks.
    fn resume_set_size(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::SetRecordSize,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_set_size(site, state, value)
    }

    /// Applies ToNumber and ToIntegerOrInfinity, saturating positive infinity explicitly.
    fn finish_set_size(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let converted = self.convert_to_number(value)?;
        let number = numeric_value(converted).ok_or(ExecutionError::InvalidSetSize(converted))?;
        if number.is_nan() {
            return Err(ExecutionError::InvalidSetSize(converted));
        }
        let integer = if number == 0.0 { 0.0 } else { number.trunc() };
        if integer.is_sign_negative() && integer != 0.0 {
            return Err(ExecutionError::NegativeSetSize(converted));
        }
        let size = if integer.is_infinite() || integer >= u64::MAX as f64 {
            u64::MAX
        } else {
            integer as u64
        };
        self.update_set_operation(state, |pending| pending.other_size = size)?;
        let other = self.set_operation_snapshot(state)?.other;
        self.get_set_operation_property(site, state, SetOperationStage::Has, other, b"has")
    }

    /// Chooses the spec branch after all four fields of GetSetRecord have been cached.
    fn start_set_operation_after_record(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let pending = self.set_operation_snapshot(state)?;
        let storage = self.set_storage(pending.receiver)?;
        let this_size = u64::from(self.collection_len(storage)?);
        match pending.kind {
            SetOperationKind::Union | SetOperationKind::SymmetricDifference => {
                self.start_set_keys_iterator(site, state)
            }
            SetOperationKind::Difference => {
                self.copy_set_receiver_result(site, state)?;
                let state = self.set_operation_state_at(site)?;
                if this_size <= pending.other_size {
                    let result = self.set_operation_snapshot(state)?.result;
                    let storage = self.set_storage(result)?;
                    let limit = self.collection_used(storage)?;
                    self.update_set_operation(state, |pending| pending.scan_limit = limit)?;
                    self.advance_set_receiver_scan(site, state)
                } else {
                    self.start_set_keys_iterator(site, state)
                }
            }
            SetOperationKind::Intersection => {
                self.create_empty_set_result(site)?;
                let state = self.set_operation_state_at(site)?;
                if this_size <= pending.other_size {
                    self.advance_set_receiver_scan(site, state)
                } else {
                    self.start_set_keys_iterator(site, state)
                }
            }
            SetOperationKind::IsSubsetOf => {
                if this_size > pending.other_size {
                    self.finish_set_boolean(site, false)
                } else {
                    self.advance_set_receiver_scan(site, state)
                }
            }
            SetOperationKind::IsSupersetOf => {
                if this_size < pending.other_size {
                    self.finish_set_boolean(site, false)
                } else {
                    self.start_set_keys_iterator(site, state)
                }
            }
            SetOperationKind::IsDisjointFrom => {
                if this_size <= pending.other_size {
                    self.advance_set_receiver_scan(site, state)
                } else {
                    self.start_set_keys_iterator(site, state)
                }
            }
        }
    }

    /// Calls cached `keys` once; cloning of union/symmetricDifference occurs only afterwards.
    fn start_set_keys_iterator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let pending = self.set_operation_snapshot(state)?;
        self.call_set_operation(
            site,
            state,
            SetOperationStage::IteratorCall,
            pending.keys,
            pending.other,
            &[],
        )
    }

    /// Caches the iterator object and observes its `next` method exactly once.
    fn resume_set_iterator_call(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return Err(ExecutionError::NotObject(value));
        }
        self.update_set_operation_value(state, |pending| &mut pending.iterator, value)?;
        self.get_set_operation_property(site, state, SetOperationStage::NextMethod, value, b"next")
    }

    /// Finishes GetIteratorFromMethod before taking operation-specific receiver snapshots.
    fn resume_set_next_method(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(value)?;
        self.update_set_operation_value(state, |pending| &mut pending.next, value)?;
        let kind = self.set_operation_snapshot(state)?.kind;
        if matches!(
            kind,
            SetOperationKind::Union | SetOperationKind::SymmetricDifference
        ) {
            self.copy_set_receiver_result(site, state)?;
        }
        let state = self.set_operation_state_at(site)?;
        self.call_set_iterator_next(site, state)
    }

    /// Calls the cached iterator `next` without re-reading it between steps.
    fn call_set_iterator_next(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let pending = self.set_operation_snapshot(state)?;
        self.call_set_operation(
            site,
            state,
            SetOperationStage::NextCall,
            pending.next,
            pending.iterator,
            &[],
        )
    }

    /// Stores one iterator result object before observing its `done` property.
    fn resume_set_next_call(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return Err(ExecutionError::NotObject(value));
        }
        self.update_set_operation_value(state, |pending| &mut pending.iterator_result, value)?;
        self.get_set_operation_property(site, state, SetOperationStage::ResultDone, value, b"done")
    }

    /// Either completes the operation or observes the iterator result's `value` property.
    fn resume_set_result_done(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_truthy_value(value)? {
            return self.finish_set_operation(site, state);
        }
        let result = self.set_operation_snapshot(state)?.iterator_result;
        self.get_set_operation_property(
            site,
            state,
            SetOperationStage::ResultValue,
            result,
            b"value",
        )
    }

    /// Canonicalizes one yielded key and applies the selected external-iterator policy.
    fn resume_set_result_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let value = self.collection_key(Some(value));
        self.update_set_operation_value(state, |pending| &mut pending.current, value)?;
        let pending = self.set_operation_snapshot(state)?;
        match pending.kind {
            SetOperationKind::Union => self.set_result_add_current(site, state)?,
            SetOperationKind::Difference => self.set_result_delete_current(state)?,
            SetOperationKind::Intersection => {
                if self.set_contains(pending.receiver, value)? {
                    self.set_result_add_current(site, state)?;
                }
            }
            SetOperationKind::SymmetricDifference => {
                if self.set_contains(pending.receiver, value)? {
                    self.set_result_delete_current(state)?;
                } else {
                    self.set_result_add_current(site, state)?;
                }
            }
            SetOperationKind::IsSupersetOf => {
                if !self.set_contains(pending.receiver, value)? {
                    return self.begin_set_normal_close(site, state);
                }
            }
            SetOperationKind::IsDisjointFrom => {
                if self.set_contains(pending.receiver, value)? {
                    return self.begin_set_normal_close(site, state);
                }
            }
            SetOperationKind::IsSubsetOf => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
        }
        let state = self.set_operation_state_at(site)?;
        self.call_set_iterator_next(site, state)
    }

    /// Advances a stable physical cursor over live receiver or fixed result-snapshot entries.
    fn advance_set_receiver_scan(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        loop {
            let pending = self.set_operation_snapshot(state)?;
            let source = if pending.kind == SetOperationKind::Difference {
                pending.result
            } else {
                pending.receiver
            };
            let storage = self.set_storage(source)?;
            let limit = if pending.kind == SetOperationKind::Difference {
                pending.scan_limit
            } else {
                self.collection_used(storage)?
            };
            if pending.cursor >= limit {
                return self.finish_set_operation(site, state);
            }
            let cursor = pending.cursor;
            self.update_set_operation(state, |pending| pending.cursor = cursor + 1)?;
            let Some(entry) = self.collection_entry(storage, cursor)? else {
                continue;
            };
            self.update_set_operation_value(state, |pending| &mut pending.current, entry.key)?;
            let pending = self.set_operation_snapshot(state)?;
            return self.call_set_operation(
                site,
                state,
                SetOperationStage::HasCall,
                pending.has,
                pending.other,
                &[pending.current],
            );
        }
    }

    /// Applies the callback result, then re-reads live receiver length on the next scan iteration.
    fn resume_set_has_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let in_other = self.is_truthy_value(value)?;
        let kind = self.set_operation_snapshot(state)?.kind;
        match kind {
            SetOperationKind::Difference if in_other => self.set_result_delete_current(state)?,
            SetOperationKind::Intersection if in_other => {
                self.set_result_add_current(site, state)?;
            }
            SetOperationKind::IsSubsetOf if !in_other => {
                return self.finish_set_boolean(site, false);
            }
            SetOperationKind::IsDisjointFrom if in_other => {
                return self.finish_set_boolean(site, false);
            }
            _ => {}
        }
        let state = self.set_operation_state_at(site)?;
        self.advance_set_receiver_scan(site, state)
    }

    /// Completes a fully consumed iterator or receiver scan with its method-specific result.
    fn finish_set_operation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let pending = self.set_operation_snapshot(state)?;
        match pending.kind {
            SetOperationKind::Union
            | SetOperationKind::Difference
            | SetOperationKind::Intersection
            | SetOperationKind::SymmetricDifference => {
                self.write(site.caller_base, site.destination, pending.result)
            }
            SetOperationKind::IsSubsetOf
            | SetOperationKind::IsSupersetOf
            | SetOperationKind::IsDisjointFrom => self.finish_set_boolean(site, true),
        }
    }

    #[inline(always)]
    fn finish_set_boolean(
        &mut self,
        site: NativeContinuationSite,
        value: bool,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(if value {
                Immediate::True
            } else {
                Immediate::False
            }),
        )
    }

    /// Starts IteratorClose with a normal completion for predicate early-return branches.
    fn begin_set_normal_close(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let iterator = self.set_operation_snapshot(state)?.iterator;
        self.get_set_operation_property(
            site,
            state,
            SetOperationStage::CloseReturnGetter,
            iterator,
            b"return",
        )
    }

    /// Calls a non-nullish iterator return method, or finishes the normal close immediately.
    fn resume_set_close_getter(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if matches!(
            value.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return self.finish_set_boolean(site, false);
        }
        self.resolve_function_object(value)?;
        let iterator = self.set_operation_snapshot(state)?.iterator;
        self.call_set_operation(
            site,
            state,
            SetOperationStage::CloseReturnCall,
            value,
            iterator,
            &[],
        )
    }

    /// Creates an intrinsic Set result with capacity sized from the receiver's live cardinality.
    fn copy_set_receiver_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.set_operation_snapshot(state)?.receiver;
        let source = self.set_storage(receiver)?;
        let live = self.collection_len(source)?;
        let used = self.collection_used(source)?;
        let prototype = self
            .realm
            .set_prototype
            .expect("Set prototype initializes before Set methods");
        let capacity = (live as usize).max(tuning::collections::INITIAL_ENTRY_CAPACITY);
        let result = self.allocate_set_object_with_capacity(prototype, capacity)?;
        let state = self.set_operation_state_at(site)?;
        self.update_set_operation_value(state, |pending| &mut pending.result, result)?;
        for index in 0..used {
            let state = self.set_operation_state_at(site)?;
            let receiver = self.set_operation_snapshot(state)?.receiver;
            let source = self.set_storage(receiver)?;
            if let Some(entry) = self.collection_entry(source, index)? {
                let result = self.set_operation_snapshot(state)?.result;
                let storage = self.set_storage(result)?;
                self.collection_append(storage, entry.key, entry.key)?;
            }
        }
        Ok(())
    }

    /// Creates an empty intrinsic Set without consulting species or public `add`.
    fn create_empty_set_result(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<(), ExecutionError> {
        let prototype = self
            .realm
            .set_prototype
            .expect("Set prototype initializes before Set methods");
        let result = self.allocate_set_object(prototype)?;
        let state = self.set_operation_state_at(site)?;
        self.update_set_operation_value(state, |pending| &mut pending.result, result)
    }

    /// Adds the rooted current key to a result, reacquiring movable state after capacity growth.
    fn set_result_add_current(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let pending = self.set_operation_snapshot(state)?;
        let storage = self.set_storage(pending.result)?;
        if self.collection_find(storage, pending.current)?.is_some() {
            return Ok(());
        }
        let storage = self.ensure_set_capacity(pending.result, storage)?;
        let state = self.set_operation_state_at(site)?;
        let pending = self.set_operation_snapshot(state)?;
        self.collection_append(storage, pending.current, pending.current)
    }

    /// Deletes the rooted current key from the private result snapshot without public calls.
    fn set_result_delete_current(
        &mut self,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        let pending = self.set_operation_snapshot(state)?;
        let storage = self.set_storage(pending.result)?;
        if let Some(index) = self.collection_find(storage, pending.current)? {
            self.collection_delete(storage, index)?;
        }
        Ok(())
    }

    #[inline(always)]
    fn set_contains(&mut self, set: Value, key: Value) -> Result<bool, ExecutionError> {
        let storage = self.set_storage(set)?;
        self.collection_find(storage, key)
            .map(|index| index.is_some())
    }

    /// Reads one protocol property through ordinary, accessor, and nested Proxy paths.
    fn get_set_operation_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        stage: SetOperationStage,
        receiver: Value,
        name: &[u8],
    ) -> Result<(), ExecutionError> {
        let key = PropertyKey::Atom(self.intern_intrinsic_name(name)?);
        let continuation = NativeContinuation::set_operation(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        match self.resolve_property_read_until_proxy(receiver, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => self.resume_set_operation(
                continuation,
                stage,
                Value::from_immediate(Immediate::Undefined),
            ),
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.resume_set_operation(continuation, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.resume_set_operation(
                    continuation,
                    stage,
                    Value::from_immediate(Immediate::Undefined),
                )
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(continuation, getter)
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                let depth = self.fiber.completions.len();
                let frames = self.fiber.frames.len();
                self.fiber
                    .completions
                    .push_native(continuation)
                    .map_err(Self::completion_stack_error)?;
                if let Err(error) =
                    self.dispatch_proxy_aware_property_read(site, receiver, receiver, key)
                {
                    if self.fiber.completions.len() > depth {
                        self.pop_native_continuation()?;
                    }
                    return Err(error);
                }
                if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
                    return Ok(());
                }
                let continuation = self.pop_native_continuation()?;
                let value = self.read(site.caller_base, site.destination)?;
                self.resume_set_operation(continuation, stage, value)
            }
        }
    }

    /// Calls one cached protocol function while the complete operation remains GC-traced.
    fn call_set_operation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
        stage: SetOperationStage,
        callee: Value,
        receiver: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(callee)?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(arguments.len())
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        copied.extend_from_slice(arguments);
        let prefix = (!copied.is_empty())
            .then(|| self.create_apply_argument_prefix(callee, receiver, copied))
            .transpose()?;
        let continuation = NativeContinuation::set_operation(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            callee,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: prefix,
            argument_prefix_offset: 0,
            argument_prefix_count: arguments.len() as u32,
            argument_count: arguments.len() as u32,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if !self
            .fiber
            .completions
            .last_native_matches(NativeContinuationKind::SetOperation(stage), site)
        {
            return Ok(());
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Set protocol callback publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let value = self.read(site.caller_base, site.destination)?;
        let continuation = self.pop_native_continuation()?;
        self.resume_set_operation(continuation, stage, value)
    }

    /// Allocates the compact traced state used by all seven methods.
    fn allocate_pending_set_operation(
        &mut self,
        pending: PendingSetOperation,
    ) -> Result<GcRef<PendingSetOperation>, ExecutionError> {
        let mut roots = PendingSetOperationRoots {
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
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_set_operation,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_set_operation_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingSetOperation>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_set_operation)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn set_operation_state_at(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<GcRef<PendingSetOperation>, ExecutionError> {
        let value = self.read(site.caller_base, site.destination)?;
        self.pending_set_operation_reference(value)
    }

    fn root_set_operation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingSetOperation>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    fn set_operation_snapshot(
        &mut self,
        state: GcRef<PendingSetOperation>,
    ) -> Result<PendingSetOperation, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_set_operation)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn update_set_operation(
        &mut self,
        state: GcRef<PendingSetOperation>,
        update: impl FnOnce(&mut PendingSetOperation),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_set_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Updates a managed edge and records the generational write barrier.
    fn update_set_operation_value(
        &mut self,
        state: GcRef<PendingSetOperation>,
        field: impl FnOnce(&mut PendingSetOperation) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_set_operation)
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
