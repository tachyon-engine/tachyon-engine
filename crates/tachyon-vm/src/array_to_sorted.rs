//! Resumable stable `Array.prototype.toSorted` collection and merge sort.

use core::{cmp::Ordering, mem::size_of};

use super::*;

/// GC-owned sort buffers and merge cursors across observable comparisons.
#[derive(Debug)]
pub(crate) struct PendingArrayToSorted {
    receiver: Value,
    result: Value,
    comparator: Value,
    left_value: Value,
    right_value: Value,
    left_string: Value,
    values: Box<[Value]>,
    scratch: Box<[Value]>,
    length: u64,
    cursor: u64,
    width: u64,
    merge_start: u64,
    left: u64,
    left_end: u64,
    right: u64,
    right_end: u64,
    destination: u64,
    active_merge: bool,
}

impl Trace for PendingArrayToSorted {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.result.trace(tracer);
        self.comparator.trace(tracer);
        self.left_value.trace(tracer);
        self.right_value.trace(tracer);
        self.left_string.trace(tracer);
        self.values.trace(tracer);
        self.scratch.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayToSorted {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.values
            .len()
            .saturating_add(self.scratch.len())
            .saturating_mul(size_of::<Value>())
    }
}

#[derive(Clone, Copy)]
struct ArrayToSortedSnapshot {
    receiver: Value,
    result: Value,
    comparator: Value,
    left_value: Value,
    right_value: Value,
    left_string: Value,
    length: u64,
    cursor: u64,
    width: u64,
    merge_start: u64,
    left: u64,
    left_end: u64,
    right: u64,
    right_end: u64,
    destination: u64,
    active_merge: bool,
}

impl Isolate {
    /// Validates comparefn before ToObject and starts the observable length Get.
    pub(crate) fn begin_array_to_sorted(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let comparator = self.call_argument(site, 0)?.unwrap_or(undefined);
        if comparator.as_immediate() != Some(Immediate::Undefined) {
            self.resolve_function_object(comparator)?;
        }
        let receiver = self.coerce_to_object(site.this_value)?;
        let state = self.allocate_array_to_sorted_state(PendingArrayToSorted {
            receiver,
            result: undefined,
            comparator,
            left_value: undefined,
            right_value: undefined,
            left_string: undefined,
            values: Box::new([]),
            scratch: Box::new([]),
            length: 0,
            cursor: 0,
            width: 1,
            merge_start: 0,
            left: 0,
            left_end: 0,
            right: 0,
            right_end: 0,
            destination: 0,
            active_merge: false,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_to_sorted_state(native_site, state)?;
        let length = self.length_atom()?;
        self.get_array_to_sorted_property(
            native_site,
            state,
            ArrayToSortedStage::Length,
            receiver,
            length.into(),
        )
    }

    /// Routes property Gets and comparator calls into the stable-sort machine.
    pub(crate) fn resume_array_to_sorted(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        stage: ArrayToSortedStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_to_sorted_state(site, state)?;
        match stage {
            ArrayToSortedStage::Length => self.resume_array_to_sorted_length(site, state, value),
            ArrayToSortedStage::SourceValue => {
                self.finish_array_to_sorted_source(site, state, value)
            }
            ArrayToSortedStage::CompareCall => {
                self.finish_array_to_sorted_compare_result(site, state, value)
            }
        }
    }

    /// Resumes length, comparator-number, and default string conversions.
    pub(crate) fn resume_array_to_sorted_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_to_sorted_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayToSortedLength => {
                self.finish_array_to_sorted_length(site, state, value)
            }
            ConversionConsumer::ArrayToSortedCompareResult => {
                self.finish_array_to_sorted_compare_number(site, state, value)
            }
            ConversionConsumer::ArrayToSortedLeftString => {
                self.finish_array_to_sorted_left_string(site, state, value)
            }
            ConversionConsumer::ArrayToSortedRightString => {
                self.finish_array_to_sorted_right_string(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts LengthOfArrayLike without retaining a Rust borrow across callbacks.
    fn resume_array_to_sorted_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayToSortedLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_to_sorted_length(site, state, value)
    }

    /// Allocates the intrinsic result and exact merge buffers after ToLength.
    fn finish_array_to_sorted_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = array_to_sorted_length(self.convert_to_number(value)?)?;
        if length > u64::from(u32::MAX) {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let old = self.array_to_sorted_snapshot(state)?;
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before toSorted");
        let result = self.create_array_object_with_prototype(prototype)?;
        let state =
            self.pending_array_to_sorted_reference(self.read(site.caller_base, site.destination)?)?;
        self.set_array_to_sorted_value(state, |pending| &mut pending.result, result)?;
        self.set_array_length_value(result, safe_integer_value(length))?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let values = exact_value_buffer(length, undefined)?;
        let scratch = exact_value_buffer(length, undefined)?;
        let state = self.allocate_array_to_sorted_state(PendingArrayToSorted {
            receiver: old.receiver,
            result,
            comparator: old.comparator,
            left_value: undefined,
            right_value: undefined,
            left_string: undefined,
            values,
            scratch,
            length,
            cursor: 0,
            width: 1,
            merge_start: 0,
            left: 0,
            left_end: 0,
            right: 0,
            right_end: 0,
            destination: 0,
            active_merge: false,
        })?;
        self.root_array_to_sorted_state(site, state)?;
        self.advance_array_to_sorted_collection(site, state)
    }

    /// Reads every source element in ascending order before any comparison.
    fn advance_array_to_sorted_collection(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_to_sorted_snapshot(state)?;
        if snapshot.cursor >= snapshot.length {
            self.update_array_to_sorted_scalars(state, |pending| pending.cursor = 0)?;
            return self.advance_array_to_sorted_merge(site, state);
        }
        let key = self.safe_integer_property_atom(snapshot.cursor)?;
        self.get_array_to_sorted_property(
            site,
            state,
            ArrayToSortedStage::SourceValue,
            snapshot.receiver,
            key.into(),
        )
    }

    /// Stores one read-through-holes value and advances the collection cursor.
    fn finish_array_to_sorted_source(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let cursor = self.array_to_sorted_snapshot(state)?.cursor;
        self.set_array_to_sorted_buffer_value(state, false, cursor, value)?;
        self.update_array_to_sorted_scalars(state, |pending| pending.cursor += 1)?;
        self.advance_array_to_sorted_collection(site, state)
    }

    /// Advances bottom-up merge sort until one observable comparison is required.
    fn advance_array_to_sorted_merge(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_to_sorted_snapshot(state)?;
            if snapshot.width >= snapshot.length {
                return self.publish_array_to_sorted(site, state);
            }
            if !snapshot.active_merge {
                if snapshot.merge_start >= snapshot.length {
                    self.finish_array_to_sorted_pass(state)?;
                    continue;
                }
                self.begin_array_to_sorted_merge(state, snapshot)?;
                continue;
            }
            if snapshot.left >= snapshot.left_end {
                self.copy_array_to_sorted_run(state, true, snapshot.right, snapshot.right_end)?;
                self.finish_array_to_sorted_merge(state, snapshot.width)?;
                continue;
            }
            if snapshot.right >= snapshot.right_end {
                self.copy_array_to_sorted_run(state, false, snapshot.left, snapshot.left_end)?;
                self.finish_array_to_sorted_merge(state, snapshot.width)?;
                continue;
            }
            let left = self.array_to_sorted_buffer_value(state, false, snapshot.left)?;
            let right = self.array_to_sorted_buffer_value(state, false, snapshot.right)?;
            self.set_array_to_sorted_value(state, |pending| &mut pending.left_value, left)?;
            self.set_array_to_sorted_value(state, |pending| &mut pending.right_value, right)?;
            return self.begin_array_to_sorted_compare(site, state);
        }
    }

    /// Initializes one adjacent pair of runs for the current merge width.
    fn begin_array_to_sorted_merge(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        snapshot: ArrayToSortedSnapshot,
    ) -> Result<(), ExecutionError> {
        let left_end = snapshot
            .merge_start
            .saturating_add(snapshot.width)
            .min(snapshot.length);
        let right_end = snapshot
            .merge_start
            .saturating_add(snapshot.width.saturating_mul(2))
            .min(snapshot.length);
        self.update_array_to_sorted_scalars(state, |pending| {
            pending.left = snapshot.merge_start;
            pending.left_end = left_end;
            pending.right = left_end;
            pending.right_end = right_end;
            pending.destination = snapshot.merge_start;
            pending.active_merge = true;
        })
    }

    /// Copies the exhausted run's counterpart without invoking the comparator.
    fn copy_array_to_sorted_run(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        right_run: bool,
        mut cursor: u64,
        end: u64,
    ) -> Result<(), ExecutionError> {
        let mut destination = self.array_to_sorted_snapshot(state)?.destination;
        while cursor < end {
            let value = self.array_to_sorted_buffer_value(state, false, cursor)?;
            self.set_array_to_sorted_buffer_value(state, true, destination, value)?;
            cursor += 1;
            destination += 1;
        }
        self.update_array_to_sorted_scalars(state, |pending| {
            pending.destination = destination;
            if right_run {
                pending.right = end;
            } else {
                pending.left = end;
            }
        })
    }

    /// Marks one merge complete and advances to the next adjacent run pair.
    fn finish_array_to_sorted_merge(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        width: u64,
    ) -> Result<(), ExecutionError> {
        self.update_array_to_sorted_scalars(state, |pending| {
            pending.merge_start = pending.merge_start.saturating_add(width.saturating_mul(2));
            pending.active_merge = false;
        })
    }

    /// Swaps complete source/destination buffers and doubles the merge width.
    fn finish_array_to_sorted_pass(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                core::mem::swap(&mut pending.values, &mut pending.scratch);
                pending.width = pending.width.saturating_mul(2);
                pending.merge_start = 0;
                pending.active_merge = false;
                Ok(())
            })
        })
    }

    /// Applies undefined ordering before dispatching user or default comparison.
    fn begin_array_to_sorted_compare(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_to_sorted_snapshot(state)?;
        let undefined = Some(Immediate::Undefined);
        if snapshot.left_value.as_immediate() == undefined {
            let ordering = if snapshot.right_value.as_immediate() == undefined {
                Ordering::Equal
            } else {
                Ordering::Greater
            };
            return self.finish_array_to_sorted_ordering(site, state, ordering);
        }
        if snapshot.right_value.as_immediate() == undefined {
            return self.finish_array_to_sorted_ordering(site, state, Ordering::Less);
        }
        if snapshot.comparator.as_immediate() == undefined {
            return self.begin_array_to_sorted_left_string(site, state);
        }
        self.call_array_to_sorted_comparator(site, state, snapshot)
    }

    /// Calls a user comparator through the iterative JS frame trampoline.
    fn call_array_to_sorted_comparator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        snapshot: ArrayToSortedSnapshot,
    ) -> Result<(), ExecutionError> {
        let receiver = Value::from_immediate(Immediate::Undefined);
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(2)
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        arguments.push(snapshot.left_value);
        arguments.push(snapshot.right_value);
        let prefix = self.create_apply_argument_prefix(snapshot.comparator, receiver, arguments)?;
        self.push_array_to_sorted_parent(
            site,
            state,
            ArrayToSortedStage::CompareCall,
            Value::from_heap_ref(prefix.raw()),
        )?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: snapshot.comparator,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 2,
            argument_count: 2,
            this_value: receiver,
            new_target: receiver,
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
                .expect("toSorted comparator publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.finish_array_to_sorted_compare_result(site, state, value)
    }

    /// Converts a user comparator result with ToNumber before choosing a run.
    fn finish_array_to_sorted_compare_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayToSortedCompareResult,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_to_sorted_compare_number(site, state, value)
    }

    /// Interprets negative comparator numbers as left-before-right; NaN is equality.
    fn finish_array_to_sorted_compare_number(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(value)?;
        let number =
            numeric_value(number).ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        let ordering = if number < 0.0 {
            Ordering::Less
        } else if number > 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        };
        self.finish_array_to_sorted_ordering(site, state, ordering)
    }

    /// Converts the left default-comparison operand with string hint.
    fn begin_array_to_sorted_left_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        let value = self.array_to_sorted_snapshot(state)?.left_value;
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayToSortedLeftString,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_to_sorted_left_string(site, state, value)
    }

    /// Stores the left default string, then converts the right operand.
    fn finish_array_to_sorted_left_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let string = self.array_to_sorted_string(value)?;
        self.set_array_to_sorted_value(state, |pending| &mut pending.left_string, string)?;
        let right = self.array_to_sorted_snapshot(state)?.right_value;
        if self.is_object_value(right) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayToSortedRightString,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                right,
                site.call_site,
            );
        }
        self.finish_array_to_sorted_right_string(site, state, right)
    }

    /// Compares the fully converted default strings by UTF-16 code units.
    fn finish_array_to_sorted_right_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let right = self.array_to_sorted_string(value)?;
        let left = self.array_to_sorted_snapshot(state)?.left_string;
        let ordering = self.compare_string_values(left, right)?;
        self.finish_array_to_sorted_ordering(site, state, ordering)
    }

    /// Implements ToString for one primitive default-comparison operand.
    fn array_to_sorted_string(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_value(Some(value))
    }

    /// Commits one stable merge choice and resumes the current run pair.
    fn finish_array_to_sorted_ordering(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        ordering: Ordering,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_to_sorted_snapshot(state)?;
        let take_left = ordering != Ordering::Greater;
        let value = if take_left {
            snapshot.left_value
        } else {
            snapshot.right_value
        };
        self.set_array_to_sorted_buffer_value(state, true, snapshot.destination, value)?;
        self.update_array_to_sorted_scalars(state, |pending| {
            pending.destination += 1;
            if take_left {
                pending.left += 1;
            } else {
                pending.right += 1;
            }
        })?;
        self.advance_array_to_sorted_merge(site, state)
    }

    /// Defines the sorted dense result after every comparison has completed.
    fn publish_array_to_sorted(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_to_sorted_snapshot(state)?;
        for index in 0..snapshot.length {
            let value = self.array_to_sorted_buffer_value(state, false, index)?;
            let key = self.safe_integer_property_atom(index)?;
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
        }
        self.write(site.caller_base, site.destination, snapshot.result)
    }

    /// Publishes a typed parent around one Proxy/accessor-aware source Get.
    fn get_array_to_sorted_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        stage: ArrayToSortedStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_to_sorted_parent(site, state, stage, receiver)?;
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
        let state = self.pending_array_to_sorted_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_array_to_sorted(site, state, stage, value)
    }

    /// Pushes one continuation that roots sort state and operation-local data.
    fn push_array_to_sorted_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
        stage: ArrayToSortedStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::array_to_sorted(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Roots sort state in the destination register before any safepoint.
    #[inline]
    fn root_array_to_sorted_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Allocates exact external sort buffers under the complete VM root set.
    fn allocate_array_to_sorted_state(
        &mut self,
        pending: PendingArrayToSorted,
    ) -> Result<GcRef<PendingArrayToSorted>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_array_to_sorted,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Validates and recovers one managed stable-sort reference.
    pub(crate) fn pending_array_to_sorted_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArrayToSorted>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_array_to_sorted)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies sort fields without retaining a managed borrow across safepoints.
    fn array_to_sorted_snapshot(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
    ) -> Result<ArrayToSortedSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(ArrayToSortedSnapshot {
                    receiver: pending.receiver,
                    result: pending.result,
                    comparator: pending.comparator,
                    left_value: pending.left_value,
                    right_value: pending.right_value,
                    left_string: pending.left_string,
                    length: pending.length,
                    cursor: pending.cursor,
                    width: pending.width,
                    merge_start: pending.merge_start,
                    left: pending.left,
                    left_end: pending.left_end,
                    right: pending.right,
                    right_end: pending.right_end,
                    destination: pending.destination,
                    active_merge: pending.active_merge,
                })
            })
        })
    }

    /// Reads one source or scratch buffer slot under a no-GC borrow.
    fn array_to_sorted_buffer_value(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        scratch: bool,
        index: u64,
    ) -> Result<Value, ExecutionError> {
        let index = usize::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let buffer = if scratch {
                    &pending.scratch
                } else {
                    &pending.values
                };
                buffer
                    .get(index)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Updates one traced buffer slot and records the owner/value barrier.
    fn set_array_to_sorted_buffer_value(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        scratch: bool,
        index: u64,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let index = usize::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let buffer = if scratch {
                    &mut pending.scratch
                } else {
                    &mut pending.values
                };
                *buffer
                    .get_mut(index)
                    .ok_or(ExecutionError::MissingNativeContinuation)? = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Updates one traced scalar edge and records its generational barrier.
    fn set_array_to_sorted_value(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        field: impl FnOnce(&mut PendingArrayToSorted) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_to_sorted)
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

    /// Updates merge cursors without requiring a write barrier.
    fn update_array_to_sorted_scalars(
        &mut self,
        state: GcRef<PendingArrayToSorted>,
        update: impl FnOnce(&mut PendingArrayToSorted),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_array_to_sorted)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }
}

fn exact_value_buffer(length: u64, value: Value) -> Result<Box<[Value]>, ExecutionError> {
    let length = usize::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(length)
        .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
    buffer.resize(length, value);
    Ok(buffer.into_boxed_slice())
}

#[inline(always)]
fn array_to_sorted_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}
