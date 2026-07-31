//! Resumable `String.raw` property observation, conversion, and assembly.

use core::mem::size_of;

use super::*;

/// GC-owned raw-template inputs and externally-accounted UTF-16 output backing.
#[derive(Debug)]
pub(crate) struct PendingStringRaw {
    raw: Value,
    retained: Value,
    substitutions: Box<[Value]>,
    output: Box<[u16]>,
    literal_count: u64,
    cursor: u64,
    output_len: usize,
}

impl Trace for PendingStringRaw {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.raw.trace(tracer);
        self.retained.trace(tracer);
        self.substitutions.trace(tracer);
    }
}

impl GcExternalMemory for PendingStringRaw {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.substitutions
            .len()
            .saturating_mul(size_of::<Value>())
            .saturating_add(self.output.len().saturating_mul(size_of::<u16>()))
    }
}

#[derive(Clone, Copy)]
struct StringRawSnapshot {
    raw: Value,
    retained: Value,
    literal_count: u64,
    cursor: u64,
    substitution_count: usize,
    output_len: usize,
    output_capacity: usize,
}

impl Isolate {
    /// Captures substitutions before observing `template.raw` and starts the property protocol.
    pub(crate) fn begin_string_raw(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let template = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let template = self.coerce_to_object(template)?;
        let substitution_count = site.argument_count.saturating_sub(1);
        let mut substitutions = Vec::new();
        substitutions
            .try_reserve_exact(substitution_count as usize)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..substitution_count {
            substitutions.push(
                self.call_argument(site, index + 1)?
                    .expect("argument count bounds String.raw substitutions"),
            );
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_string_raw_state(PendingStringRaw {
            raw: template,
            retained: undefined,
            substitutions: substitutions.into_boxed_slice(),
            output: Box::new([]),
            literal_count: 0,
            cursor: 0,
            output_len: 0,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_string_raw_state(native_site, state)?;
        let raw = self.intern_intrinsic_name(b"raw")?;
        let rooted = self.read(native_site.caller_base, native_site.destination)?;
        let state = self.pending_string_raw_reference(rooted)?;
        let template = self.string_raw_snapshot(state)?.raw;
        if let Some((state, value)) = self.dispatch_string_raw_get(
            native_site,
            state,
            StringRawStage::Raw,
            template,
            raw.into(),
        )? {
            self.resume_string_raw_stage(native_site, state, StringRawStage::Raw, value)?;
        }
        Ok(())
    }

    /// Routes a completed Proxy/accessor-aware property read back into the raw-template driver.
    pub(crate) fn resume_string_raw(
        &mut self,
        continuation: NativeContinuation,
        stage: StringRawStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_string_raw_reference(continuation.first())?;
        self.resume_string_raw_stage(continuation.site(), state, stage, value)
    }

    /// Routes object-to-primitive completions back to length or string conversion stages.
    pub(crate) fn resume_string_raw_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        consumer: ConversionConsumer,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.set_string_raw_retained(state, primitive)?;
        self.root_string_raw_state(site, state)?;
        match consumer {
            ConversionConsumer::StringRawLength => {
                self.finish_string_raw_length(site, state, primitive)
            }
            ConversionConsumer::StringRawLiteral => {
                let state = self.append_string_raw_string(site, state, primitive)?;
                let Some(state) = self.after_string_raw_literal(site, state)? else {
                    return Ok(());
                };
                self.advance_string_raw(site, state)
            }
            ConversionConsumer::StringRawSubstitution => {
                let state = self.append_string_raw_string(site, state, primitive)?;
                self.increment_string_raw_cursor(state)?;
                self.advance_string_raw(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Stores each observed value before any conversion allocation and advances its exact stage.
    fn resume_string_raw_stage(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        stage: StringRawStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_string_raw_retained(state, value)?;
        self.root_string_raw_state(site, state)?;
        match stage {
            StringRawStage::Raw => {
                let raw = self.coerce_to_object(value)?;
                let rooted = self.read(site.caller_base, site.destination)?;
                let state = self.pending_string_raw_reference(rooted)?;
                self.set_string_raw_raw(state, raw)?;
                let length = self.length_atom()?;
                let rooted = self.read(site.caller_base, site.destination)?;
                let state = self.pending_string_raw_reference(rooted)?;
                let raw = self.string_raw_snapshot(state)?.raw;
                if let Some((state, value)) = self.dispatch_string_raw_get(
                    site,
                    state,
                    StringRawStage::Length,
                    raw,
                    length.into(),
                )? {
                    self.resume_string_raw_stage(site, state, StringRawStage::Length, value)?;
                }
                Ok(())
            }
            StringRawStage::Length => self.convert_string_raw_value(
                site,
                state,
                ConversionConsumer::StringRawLength,
                value,
            ),
            StringRawStage::Element => {
                let Some(state) = self.process_string_raw_literal(site, state, value)? else {
                    return Ok(());
                };
                self.advance_string_raw(site, state)
            }
        }
    }

    /// Runs synchronous indexed Gets and primitive conversions on a constant Rust stack.
    fn advance_string_raw(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingStringRaw>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.string_raw_snapshot(state)?;
            if snapshot.cursor >= snapshot.literal_count {
                return self.finish_string_raw(site, state);
            }
            let key = self.safe_integer_property_atom(snapshot.cursor)?;
            let rooted = self.read(site.caller_base, site.destination)?;
            state = self.pending_string_raw_reference(rooted)?;
            let snapshot = self.string_raw_snapshot(state)?;
            let Some((completed, value)) = self.dispatch_string_raw_get(
                site,
                state,
                StringRawStage::Element,
                snapshot.raw,
                key.into(),
            )?
            else {
                return Ok(());
            };
            let Some(completed) = self.process_string_raw_literal(site, completed, value)? else {
                return Ok(());
            };
            state = completed;
        }
    }

    /// Converts one raw segment, suspending only when its ToString executes JavaScript.
    fn process_string_raw_literal(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        value: Value,
    ) -> Result<Option<GcRef<PendingStringRaw>>, ExecutionError> {
        self.set_string_raw_retained(state, value)?;
        self.root_string_raw_state(site, state)?;
        if self.is_object_value(value) {
            self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringRawLiteral,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            )?;
            return Ok(None);
        }
        let state = self.append_string_raw_string(site, state, value)?;
        self.after_string_raw_literal(site, state)
    }

    /// Inserts the current substitution when one exists, otherwise advances to the next segment.
    fn after_string_raw_literal(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
    ) -> Result<Option<GcRef<PendingStringRaw>>, ExecutionError> {
        let snapshot = self.string_raw_snapshot(state)?;
        if snapshot.cursor + 1 >= snapshot.literal_count {
            self.finish_string_raw(site, state)?;
            return Ok(None);
        }
        let Some(substitution) = self.string_raw_substitution(state, snapshot.cursor as usize)?
        else {
            self.increment_string_raw_cursor(state)?;
            return Ok(Some(state));
        };
        self.set_string_raw_retained(state, substitution)?;
        self.root_string_raw_state(site, state)?;
        if self.is_object_value(substitution) {
            self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringRawSubstitution,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                substitution,
                site.call_site,
            )?;
            return Ok(None);
        }
        let state = self.append_string_raw_string(site, state, substitution)?;
        self.increment_string_raw_cursor(state)?;
        Ok(Some(state))
    }

    /// Converts the observed raw length with number hint and installs capacity-planned backing.
    fn finish_string_raw_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(primitive)?;
        let literal_count = crate::regexp_exec::regexp_to_length(number)?;
        let snapshot = self.string_raw_snapshot(state)?;
        let segment_count = literal_count.saturating_add(
            literal_count
                .saturating_sub(1)
                .min(snapshot.substitution_count as u64),
        );
        let estimated = usize::try_from(segment_count)
            .unwrap_or(usize::MAX)
            .saturating_mul(tuning::strings::RAW_INITIAL_UNITS_PER_SEGMENT)
            .min(tuning::strings::RAW_MAX_INITIAL_UNITS);
        let substitutions = self.string_raw_substitutions(state)?;
        let output = exact_string_raw_buffer(estimated)?;
        let replacement = self.allocate_string_raw_state(PendingStringRaw {
            raw: snapshot.raw,
            retained: Value::from_immediate(Immediate::Undefined),
            substitutions: substitutions.into_boxed_slice(),
            output,
            literal_count,
            cursor: 0,
            output_len: 0,
        })?;
        self.root_string_raw_state(site, replacement)?;
        self.advance_string_raw(site, replacement)
    }

    /// Converts immediately primitive operands or dispatches their observable ToPrimitive path.
    fn convert_string_raw_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                consumer,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.resume_string_raw_conversion(site, state, consumer, value)
    }

    /// Applies ToString and appends its exact UTF-16 units to managed state backing.
    fn append_string_raw_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        primitive: Value,
    ) -> Result<GcRef<PendingStringRaw>, ExecutionError> {
        self.root_string_raw_state(site, state)?;
        let string = self.primitive_to_string_value(primitive)?;
        let rooted = self.read(site.caller_base, site.destination)?;
        let state = self.pending_string_raw_reference(rooted)?;
        self.set_string_raw_retained(state, string)?;
        let capacity = self.primitive_string_unit_length(string)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        self.append_primitive_string_units(string, &mut units)?;
        self.append_string_raw_units(site, state, &units)
    }

    /// Appends one unit slice after replacing externally-accounted backing when required.
    fn append_string_raw_units(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        units: &[u16],
    ) -> Result<GcRef<PendingStringRaw>, ExecutionError> {
        let required = self
            .string_raw_snapshot(state)?
            .output_len
            .checked_add(units.len())
            .filter(|length| *length <= u32::MAX as usize)
            .ok_or(ExecutionError::InvalidStringLength)?;
        let state = self.ensure_string_raw_capacity(site, state, required)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_string_raw)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let end = pending.output_len + units.len();
                pending.output[pending.output_len..end].copy_from_slice(units);
                pending.output_len = end;
                Ok(())
            })
        })?;
        Ok(state)
    }

    /// Allocates a charged replacement state when the current fixed output backing is full.
    fn ensure_string_raw_capacity(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        required: usize,
    ) -> Result<GcRef<PendingStringRaw>, ExecutionError> {
        let snapshot = self.string_raw_snapshot(state)?;
        if required <= snapshot.output_capacity {
            return Ok(state);
        }
        let capacity = tuning::strings::grown_raw_capacity(snapshot.output_capacity, required)
            .filter(|capacity| *capacity <= u32::MAX as usize)
            .ok_or(ExecutionError::InvalidStringLength)?;
        let substitutions = self.string_raw_substitutions(state)?;
        let committed = self.string_raw_output_units(state)?;
        let mut output = exact_string_raw_buffer(capacity)?;
        output[..committed.len()].copy_from_slice(&committed);
        let replacement = self.allocate_string_raw_state(PendingStringRaw {
            raw: snapshot.raw,
            retained: snapshot.retained,
            substitutions: substitutions.into_boxed_slice(),
            output,
            literal_count: snapshot.literal_count,
            cursor: snapshot.cursor,
            output_len: snapshot.output_len,
        })?;
        self.root_string_raw_state(site, replacement)?;
        Ok(replacement)
    }

    /// Publishes one native parent around an accessor/Proxy-aware property read.
    fn dispatch_string_raw_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
        stage: StringRawStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<(GcRef<PendingStringRaw>, Value)>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::string_raw(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                receiver,
            ))
            .map_err(Self::completion_stack_error)?;
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
        let continuation = self.pop_native_continuation()?;
        let state = self.pending_string_raw_reference(continuation.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        Ok(Some((state, value)))
    }

    /// Allocates one externally-accounted state while VM roots cover every source edge.
    fn allocate_string_raw_state(
        &mut self,
        pending: PendingStringRaw,
    ) -> Result<GcRef<PendingStringRaw>, ExecutionError> {
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
                self.types.pending_string_raw,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked String.raw state reference from a managed Value.
    pub(crate) fn pending_string_raw_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingStringRaw>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_string_raw)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Roots state in the native call destination before crossing any safepoint.
    #[inline]
    fn root_string_raw_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Copies traced edges and scalar cursors without retaining a managed borrow.
    fn string_raw_snapshot(
        &mut self,
        state: GcRef<PendingStringRaw>,
    ) -> Result<StringRawSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_string_raw)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(StringRawSnapshot {
                    raw: pending.raw,
                    retained: pending.retained,
                    literal_count: pending.literal_count,
                    cursor: pending.cursor,
                    substitution_count: pending.substitutions.len(),
                    output_len: pending.output_len,
                    output_capacity: pending.output.len(),
                })
            })
        })
    }

    /// Replaces the active raw-object edge and records its generational barrier.
    fn set_string_raw_raw(
        &mut self,
        state: GcRef<PendingStringRaw>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_string_raw_value(state, value, |pending| &mut pending.raw)
    }

    /// Replaces the temporary retained edge and records its generational barrier.
    fn set_string_raw_retained(
        &mut self,
        state: GcRef<PendingStringRaw>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_string_raw_value(state, value, |pending| &mut pending.retained)
    }

    /// Updates one traced field through a shared no-GC borrow and barrier sequence.
    fn set_string_raw_value(
        &mut self,
        state: GcRef<PendingStringRaw>,
        value: Value,
        select: impl FnOnce(&mut PendingStringRaw) -> &mut Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_string_raw)
                    .map_err(ExecutionError::NoGcBorrow)?;
                *select(pending) = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Advances the literal cursor after its optional substitution has been appended.
    fn increment_string_raw_cursor(
        &mut self,
        state: GcRef<PendingStringRaw>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_string_raw)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.cursor += 1;
                Ok(())
            })
        })
    }

    /// Copies one substitution edge without retaining a managed borrow.
    fn string_raw_substitution(
        &mut self,
        state: GcRef<PendingStringRaw>,
        index: usize,
    ) -> Result<Option<Value>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_string_raw)
                    .map(|pending| pending.substitutions.get(index).copied())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Copies all immutable substitution edges for a replacement state allocation.
    fn string_raw_substitutions(
        &mut self,
        state: GcRef<PendingStringRaw>,
    ) -> Result<Vec<Value>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_string_raw)
                    .map(|pending| pending.substitutions.to_vec())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Copies committed output for charged backing replacement or final String allocation.
    fn string_raw_output_units(
        &mut self,
        state: GcRef<PendingStringRaw>,
    ) -> Result<Vec<u16>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_string_raw)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(pending.output[..pending.output_len].to_vec())
            })
        })
    }

    /// Allocates the final exact String after all required raw segments are observed.
    fn finish_string_raw(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringRaw>,
    ) -> Result<(), ExecutionError> {
        self.root_string_raw_state(site, state)?;
        let units = self.string_raw_output_units(state)?;
        let string = JsString::try_from_owned_code_units(units)
            .map_err(ExecutionError::PropertyKeyString)?;
        let result = self.allocate_runtime_string(string)?;
        self.write(site.caller_base, site.destination, result)
    }
}

/// Allocates an exact fixed output buffer whose full size is charged to the GC heap.
fn exact_string_raw_buffer(capacity: usize) -> Result<Box<[u16]>, ExecutionError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    output.resize(capacity, 0);
    Ok(output.into_boxed_slice())
}
