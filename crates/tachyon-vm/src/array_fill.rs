//! Resumable `Array.prototype.fill` algorithm.

use super::*;

mod support;

/// GC-owned inputs and cursors across observable fill operations.
#[derive(Debug)]
pub(crate) struct PendingArrayFill {
    receiver: Value,
    value: Value,
    start_argument: Value,
    end_argument: Value,
    length: u64,
    cursor: u64,
    end: u64,
}

impl Trace for PendingArrayFill {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.value.trace(tracer);
        self.start_argument.trace(tracer);
        self.end_argument.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayFill {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

#[derive(Clone, Copy)]
struct ArrayFillSnapshot {
    receiver: Value,
    value: Value,
    start_argument: Value,
    end_argument: Value,
    length: u64,
    cursor: u64,
    end: u64,
}

impl Isolate {
    /// Captures arguments before beginning the observable receiver length lookup.
    pub(crate) fn begin_array_fill(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let value = self.call_argument(site, 0)?.unwrap_or(undefined);
        let start_argument = self.call_argument(site, 1)?.unwrap_or(undefined);
        let end_argument = self.call_argument(site, 2)?.unwrap_or(undefined);
        let state = self.allocate_array_fill_state(PendingArrayFill {
            receiver,
            value,
            start_argument,
            end_argument,
            length: 0,
            cursor: 0,
            end: 0,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_fill_state(native_site, state)?;
        let length = self.length_atom()?;
        let observed = self.dispatch_array_fill_get(
            native_site,
            state,
            ArrayFillStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some((state, value)) = observed {
            self.resume_array_fill(native_site, state, ArrayFillStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes each observable completion into the fill state machine.
    pub(crate) fn resume_array_fill(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        stage: ArrayFillStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_fill_state(site, state)?;
        match stage {
            ArrayFillStage::Length => self.resume_array_fill_length(site, state, value),
            ArrayFillStage::Set => self.finish_array_fill_set(site, state),
        }
    }

    /// Resumes one length/start/end object-to-primitive conversion.
    pub(crate) fn resume_array_fill_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_fill_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayFillLength => {
                self.finish_array_fill_length(site, state, value)
            }
            ConversionConsumer::ArrayFillStart => self.finish_array_fill_start(site, state, value),
            ConversionConsumer::ArrayFillEnd => self.finish_array_fill_end(site, state, value),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts the observed length while allowing user ToPrimitive code.
    fn resume_array_fill_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.convert_array_fill_value(site, state, ConversionConsumer::ArrayFillLength, value)
    }

    /// Stores ToLength and begins start conversion.
    fn finish_array_fill_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = array_fill_to_length(self.convert_to_number(value)?)?;
        self.update_array_fill_scalars(state, |pending| pending.length = length)?;
        let start = self.array_fill_snapshot(state)?.start_argument;
        self.convert_array_fill_value(site, state, ConversionConsumer::ArrayFillStart, start)
    }

    /// Stores normalized start and begins optional end conversion.
    fn finish_array_fill_start(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let relative = array_fill_integer(self.convert_to_number(value)?)?;
        let snapshot = self.array_fill_snapshot(state)?;
        let start = array_fill_relative_index(relative, snapshot.length);
        self.update_array_fill_scalars(state, |pending| pending.cursor = start)?;
        if snapshot.end_argument.as_immediate() == Some(Immediate::Undefined) {
            self.update_array_fill_scalars(state, |pending| pending.end = snapshot.length)?;
            return self.advance_array_fill(site, state);
        }
        self.convert_array_fill_value(
            site,
            state,
            ConversionConsumer::ArrayFillEnd,
            snapshot.end_argument,
        )
    }

    /// Stores normalized end before the first indexed Set.
    fn finish_array_fill_end(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let relative = array_fill_integer(self.convert_to_number(value)?)?;
        let length = self.array_fill_snapshot(state)?.length;
        let end = array_fill_relative_index(relative, length);
        self.update_array_fill_scalars(state, |pending| pending.end = end)?;
        self.advance_array_fill(site, state)
    }

    /// Runs synchronously completed Sets in a loop so range size cannot grow the Rust stack.
    fn advance_array_fill(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayFill>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_fill_snapshot(state)?;
            if snapshot.cursor >= snapshot.end {
                return self.write(site.caller_base, site.destination, snapshot.receiver);
            }
            let key = self.safe_integer_property_atom(snapshot.cursor)?;
            let Some(completed) = self.dispatch_array_fill_set(
                site,
                state,
                ArrayFillStage::Set,
                snapshot.receiver,
                key.into(),
                snapshot.value,
            )?
            else {
                return Ok(());
            };
            state = completed;
            self.commit_array_fill_set(state)?;
        }
    }

    /// Commits a suspended Set and returns to the explicit loop.
    fn finish_array_fill_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
    ) -> Result<(), ExecutionError> {
        self.commit_array_fill_set(state)?;
        self.advance_array_fill(site, state)
    }

    /// Converts one input immediately or dispatches its object ToPrimitive operation.
    fn convert_array_fill_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFill>,
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
        self.resume_array_fill_conversion(site, state, consumer, value)
    }
}

#[inline(always)]
fn array_fill_to_length(value: Value) -> Result<u64, ExecutionError> {
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
fn array_fill_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    })
}

#[inline(always)]
fn array_fill_relative_index(relative: f64, length: u64) -> u64 {
    if relative == f64::NEG_INFINITY {
        return 0;
    }
    if relative < 0.0 {
        return length.saturating_sub((-relative).min(length as f64) as u64);
    }
    if !relative.is_finite() || relative >= length as f64 {
        return length;
    }
    relative as u64
}
