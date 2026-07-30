//! Resumable `String.prototype.concat` argument conversion and assembly.

use core::mem::size_of;

use super::*;

/// GC-owned argument vector retained while observable ToString callbacks run.
#[derive(Debug)]
pub(crate) struct PendingStringConcat {
    values: Vec<Value>,
    cursor: usize,
}

impl Trace for PendingStringConcat {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.values.trace(tracer);
    }
}

impl GcExternalMemory for PendingStringConcat {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.values.capacity().saturating_mul(size_of::<Value>())
    }
}

impl Isolate {
    /// Copies the exact receiver/argument window before starting left-to-right ToString conversion.
    pub(crate) fn begin_string_concat(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        if is_nullish(site.this_value) {
            return Err(ExecutionError::NotObject(site.this_value));
        }
        let count = usize::try_from(site.argument_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count.saturating_add(1))
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        values.push(site.this_value);
        for index in 0..site.argument_count {
            values.push(
                self.call_argument(site, index)?
                    .expect("argument count bounds the concat window"),
            );
        }
        let state = self.allocate_string_concat_state(PendingStringConcat { values, cursor: 0 })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_string_concat(continuation_site, state)
    }

    /// Stores one completed conversion and resumes the constant-Rust-stack concat driver.
    pub(crate) fn resume_string_concat_conversion(
        &mut self,
        site: NativeContinuationSite,
        state_value: Value,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.write(site.caller_base, site.destination, state_value)?;
        let string = self.primitive_to_string_value(primitive)?;
        let rooted_state = self.read(site.caller_base, site.destination)?;
        let state = self.pending_string_concat_reference(rooted_state)?;
        self.update_string_concat_value(state, string)?;
        self.advance_string_concat(site, state)
    }

    /// Converts arguments left-to-right, suspending only around observable object conversion.
    fn advance_string_concat(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringConcat>,
    ) -> Result<(), ExecutionError> {
        loop {
            let Some(value) = self.string_concat_cursor_value(state)? else {
                let result = self.finish_string_concat(state)?;
                return self.write(site.caller_base, site.destination, result);
            };
            if self.is_object_value(value) {
                return self.dispatch_object_primitive_conversion(
                    ConversionConsumer::StringConcatElement,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    value,
                    site.call_site,
                );
            }
            let string = self.primitive_to_string_value(value)?;
            self.update_string_concat_value(state, string)?;
        }
    }

    /// Builds the final String with one exact UTF-16 capacity allocation.
    fn finish_string_concat(
        &mut self,
        state: GcRef<PendingStringConcat>,
    ) -> Result<Value, ExecutionError> {
        let len = self.string_concat_len(state)?;
        let mut capacity = 0_usize;
        for index in 0..len {
            let value = self.string_concat_value(state, index)?;
            capacity = capacity
                .checked_add(self.string_value_length(value)?)
                .filter(|length| *length <= u32::MAX as usize)
                .ok_or(ExecutionError::InvalidStringLength)?;
        }
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..len {
            let value = self.string_concat_value(state, index)?;
            self.append_primitive_string_units(value, &mut units)?;
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Allocates the external argument backing while the call frame still roots every input.
    fn allocate_string_concat_state(
        &mut self,
        pending: PendingStringConcat,
    ) -> Result<GcRef<PendingStringConcat>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_string_concat,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn pending_string_concat_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingStringConcat>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_string_concat)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn string_concat_cursor_value(
        &mut self,
        state: GcRef<PendingStringConcat>,
    ) -> Result<Option<Value>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_string_concat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(pending.values.get(pending.cursor).copied())
            })
        })
    }

    fn string_concat_len(
        &mut self,
        state: GcRef<PendingStringConcat>,
    ) -> Result<usize, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_string_concat)
                    .map(|pending| pending.values.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn string_concat_value(
        &mut self,
        state: GcRef<PendingStringConcat>,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_string_concat)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values
                    .get(index)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Replaces the current argument edge, advances the cursor, and records the barrier.
    fn update_string_concat_value(
        &mut self,
        state: GcRef<PendingStringConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_string_concat)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let slot = pending
                    .values
                    .get_mut(pending.cursor)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                *slot = value;
                pending.cursor += 1;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}
