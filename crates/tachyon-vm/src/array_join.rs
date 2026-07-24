//! Resumable `Array.prototype.join` string assembly.

use core::mem::size_of;

use super::*;

mod support;

/// GC-owned join inputs, output backing, and observable cursor.
#[derive(Debug)]
pub(crate) struct PendingArrayJoin {
    receiver: Value,
    separator_argument: Value,
    retained: Value,
    separator: Box<[u16]>,
    output: Box<[u16]>,
    length: u64,
    cursor: u64,
    output_len: usize,
    locale: bool,
}

impl Trace for PendingArrayJoin {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.separator_argument.trace(tracer);
        self.retained.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayJoin {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.separator
            .len()
            .saturating_add(self.output.len())
            .saturating_mul(size_of::<u16>())
    }
}

#[derive(Clone, Copy)]
struct ArrayJoinSnapshot {
    receiver: Value,
    separator_argument: Value,
    retained: Value,
    length: u64,
    cursor: u64,
    output_len: usize,
    output_capacity: usize,
    locale: bool,
}

impl Isolate {
    /// Captures receiver and separator before the observable length lookup.
    pub(crate) fn begin_array_join(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_array_join_mode(site, false)
    }

    /// Begins `Array.prototype.toLocaleString` with per-element method calls.
    pub(crate) fn begin_array_to_locale_string(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_array_join_mode(site, true)
    }

    /// Captures shared join state before the observable length lookup.
    fn begin_array_join_mode(
        &mut self,
        site: &CallSite,
        locale: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let separator_argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let state = self.allocate_array_join_state(PendingArrayJoin {
            receiver,
            separator_argument,
            retained: undefined,
            separator: Box::new([]),
            output: Box::new([]),
            length: 0,
            cursor: 0,
            output_len: 0,
            locale,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_join_state(continuation_site, state)?;
        let length = self.length_atom()?;
        if let Some((state, value)) = self.dispatch_array_join_get(
            continuation_site,
            state,
            ArrayJoinStage::Length,
            receiver,
            length.into(),
        )? {
            self.resume_array_join(continuation_site, state, ArrayJoinStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes observable Get completions into the join state machine.
    pub(crate) fn resume_array_join(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        stage: ArrayJoinStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if stage != ArrayJoinStage::ElementLocaleGet {
            self.set_array_join_retained(state, value)?;
        }
        self.root_array_join_state(site, state)?;
        match stage {
            ArrayJoinStage::Length => self.resume_array_join_length(site, state, value),
            ArrayJoinStage::ElementGet => self.finish_array_join_element(site, state, value),
            ArrayJoinStage::ElementLocaleGet => {
                self.finish_array_join_locale_get(site, state, value)
            }
            ArrayJoinStage::ElementLocaleCall => {
                self.finish_array_join_locale_call(site, state, value)
            }
        }
    }

    /// Routes length, separator, and element conversions back to join.
    pub(crate) fn resume_array_join_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_join_retained(state, value)?;
        self.root_array_join_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayJoinLength => {
                self.finish_array_join_length(site, state, value)
            }
            ConversionConsumer::ArrayJoinSeparator => {
                self.finish_array_join_separator(site, state, value)
            }
            ConversionConsumer::ArrayJoinElement => {
                self.finish_array_join_element_string(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts the observed length while allowing user ToPrimitive code.
    fn resume_array_join_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.convert_array_join_value(site, state, ConversionConsumer::ArrayJoinLength, value)
    }

    /// Stores ToLength and starts separator ToString in specification order.
    fn finish_array_join_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = array_join_to_length(self.convert_to_number(value)?)?;
        self.update_array_join_scalars(state, |pending| pending.length = length)?;
        let separator = self.array_join_snapshot(state)?.separator_argument;
        if separator.as_immediate() == Some(Immediate::Undefined) {
            return self.install_array_join_separator(site, state, vec![u16::from(b',')]);
        }
        self.convert_array_join_value(
            site,
            state,
            ConversionConsumer::ArrayJoinSeparator,
            separator,
        )
    }

    /// Materializes a primitive separator and installs its exact backing.
    fn finish_array_join_separator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let units = self.array_join_primitive_units(value)?;
        self.install_array_join_separator(site, state, units)
    }

    /// Replaces the bootstrap state with capacity-planned separator/output backing.
    fn install_array_join_separator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        separator: Vec<u16>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_join_snapshot(state)?;
        let per_element =
            tuning::arrays::JOIN_INITIAL_UNITS_PER_ELEMENT.saturating_add(separator.len());
        let estimated = usize::try_from(snapshot.length)
            .unwrap_or(usize::MAX)
            .saturating_mul(per_element)
            .min(tuning::arrays::JOIN_MAX_INITIAL_UNITS);
        let output = exact_array_join_buffer(estimated)?;
        let replacement = self.allocate_array_join_state(PendingArrayJoin {
            receiver: snapshot.receiver,
            separator_argument: snapshot.separator_argument,
            retained: Value::from_immediate(Immediate::Undefined),
            separator: separator.into_boxed_slice(),
            output,
            length: snapshot.length,
            cursor: 0,
            output_len: 0,
            locale: snapshot.locale,
        })?;
        self.root_array_join_state(site, replacement)?;
        self.advance_array_join(site, replacement)
    }

    /// Runs synchronous element Gets and primitive conversions without Rust recursion.
    fn advance_array_join(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayJoin>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_join_snapshot(state)?;
            if snapshot.cursor >= snapshot.length {
                return self.finish_array_join_output(site, state);
            }
            if snapshot.cursor != 0 {
                state = self.append_array_join_separator(site, state)?;
            }
            let snapshot = self.array_join_snapshot(state)?;
            let index = snapshot.cursor;
            self.update_array_join_scalars(state, |pending| pending.cursor += 1)?;
            let key = self.safe_integer_property_atom(index)?;
            let Some((completed, value)) = self.dispatch_array_join_get(
                site,
                state,
                ArrayJoinStage::ElementGet,
                snapshot.receiver,
                key.into(),
            )?
            else {
                return Ok(());
            };
            state = completed;
            let Some(completed) = self.process_array_join_element(site, state, value)? else {
                return Ok(());
            };
            state = completed;
        }
    }

    /// Processes one resumed indexed Get before continuing the explicit loop.
    fn finish_array_join_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let Some(state) = self.process_array_join_element(site, state, value)? else {
            return Ok(());
        };
        self.advance_array_join(site, state)
    }

    /// Skips nullish/self values or starts their string conversion.
    fn process_array_join_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<Option<GcRef<PendingArrayJoin>>, ExecutionError> {
        let receiver = self.array_join_snapshot(state)?.receiver;
        if value == receiver
            || matches!(
                value.as_immediate(),
                Some(Immediate::Undefined | Immediate::Null)
            )
        {
            return Ok(Some(state));
        }
        if self.array_join_snapshot(state)?.locale {
            let key = self.intern_intrinsic_name(b"toLocaleString")?;
            let Some((state, callee)) = self.dispatch_array_join_get(
                site,
                state,
                ArrayJoinStage::ElementLocaleGet,
                value,
                key.into(),
            )?
            else {
                return Ok(None);
            };
            self.finish_array_join_locale_get(site, state, callee)?;
            return Ok(None);
        }
        if self.is_object_value(value) {
            self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayJoinElement,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            )?;
            return Ok(None);
        }
        self.finish_array_join_element_string_value(site, state, value)
            .map(Some)
    }

    /// Appends a resumed element ToString result and returns to the join loop.
    fn finish_array_join_element_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.finish_array_join_element_string_value(site, state, value)?;
        self.advance_array_join(site, state)
    }

    /// Looks up an element's `toLocaleString` method before invoking it.
    fn finish_array_join_locale_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        callee: Value,
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(callee)?;
        let element = self.array_join_snapshot(state)?.retained;
        self.dispatch_property_callback(
            NativeContinuation::array_join(
                site,
                ArrayJoinStage::ElementLocaleCall,
                Value::from_heap_ref(state.raw()),
                element,
            ),
            callee,
        )?;
        Ok(())
    }

    /// Converts a locale method result to string without another locale lookup.
    fn finish_array_join_locale_call(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayJoinElement,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        let state = self.finish_array_join_element_string_value(site, state, value)?;
        self.advance_array_join(site, state)
    }

    /// Appends one already-primitive element string representation.
    fn finish_array_join_element_string_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
        value: Value,
    ) -> Result<GcRef<PendingArrayJoin>, ExecutionError> {
        let units = self.array_join_primitive_units(value)?;
        self.append_array_join_units(site, state, &units)
    }

    /// Converts one input immediately or dispatches string-hint ToPrimitive.
    fn convert_array_join_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayJoin>,
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
        self.resume_array_join_conversion(site, state, consumer, value)
    }

    /// Implements abstract ToString for an already-primitive join operand.
    fn array_join_primitive_units(&mut self, value: Value) -> Result<Vec<u16>, ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        let capacity = self.primitive_string_unit_length(value)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        self.append_primitive_string_units(value, &mut units)?;
        debug_assert_eq!(units.len(), capacity);
        Ok(units)
    }
}

#[inline(always)]
fn array_join_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}

fn exact_array_join_buffer(capacity: usize) -> Result<Box<[u16]>, ExecutionError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    buffer.resize(capacity, 0);
    Ok(buffer.into_boxed_slice())
}
