//! Resumable `Array.prototype.copyWithin` algorithm.

use super::*;

mod support;

/// GC-owned arguments and traversal state across observable JavaScript work.
#[derive(Debug)]
pub(crate) struct PendingArrayCopyWithin {
    receiver: Value,
    retained: Value,
    target_argument: Value,
    start_argument: Value,
    end_argument: Value,
    length: u64,
    from: u64,
    to: u64,
    count: u64,
    direction: i8,
}

impl Trace for PendingArrayCopyWithin {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.retained.trace(tracer);
        self.target_argument.trace(tracer);
        self.start_argument.trace(tracer);
        self.end_argument.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayCopyWithin {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

#[derive(Clone, Copy)]
struct ArrayCopyWithinSnapshot {
    receiver: Value,
    target_argument: Value,
    start_argument: Value,
    end_argument: Value,
    length: u64,
    from: u64,
    to: u64,
    count: u64,
}

impl Isolate {
    /// Captures arguments before beginning the observable receiver length lookup.
    pub(crate) fn begin_array_copy_within(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let target_argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let start_argument = self.call_argument(site, 1)?.unwrap_or(undefined);
        let end_argument = self.call_argument(site, 2)?.unwrap_or(undefined);
        let state = self.allocate_array_copy_within_state(PendingArrayCopyWithin {
            receiver,
            retained: undefined,
            target_argument,
            start_argument,
            end_argument,
            length: 0,
            from: 0,
            to: 0,
            count: 0,
            direction: 1,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_copy_within_state(native_site, state)?;
        let length = self.length_atom()?;
        let observed = self.dispatch_array_copy_within_get(
            native_site,
            state,
            ArrayCopyWithinStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some((state, value)) = observed {
            self.resume_array_copy_within(native_site, state, ArrayCopyWithinStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes observable completions into the copyWithin state machine.
    pub(crate) fn resume_array_copy_within(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        stage: ArrayCopyWithinStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_copy_within_state(site, state)?;
        match stage {
            ArrayCopyWithinStage::Length => {
                self.resume_array_copy_within_length(site, state, value)
            }
            ArrayCopyWithinStage::MoveHas => self.finish_array_copy_within_has(site, state, value),
            ArrayCopyWithinStage::MoveGet => self.finish_array_copy_within_get(site, state, value),
            ArrayCopyWithinStage::MoveSet | ArrayCopyWithinStage::MoveDelete => {
                self.finish_array_copy_within_move(site, state)
            }
        }
    }

    /// Resumes one object-to-primitive argument conversion in specification order.
    pub(crate) fn resume_array_copy_within_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_copy_within_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayCopyWithinLength => {
                self.finish_array_copy_within_length(site, state, value)
            }
            ConversionConsumer::ArrayCopyWithinTarget => {
                self.finish_array_copy_within_target(site, state, value)
            }
            ConversionConsumer::ArrayCopyWithinStart => {
                self.finish_array_copy_within_start(site, state, value)
            }
            ConversionConsumer::ArrayCopyWithinEnd => {
                self.finish_array_copy_within_end(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts the observed length, allowing user code during ToPrimitive.
    fn resume_array_copy_within_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_array_copy_within_conversion(
                site,
                state,
                ConversionConsumer::ArrayCopyWithinLength,
                value,
            );
        }
        self.finish_array_copy_within_length(site, state, value)
    }

    /// Stores ToLength and begins target conversion.
    fn finish_array_copy_within_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = copy_within_to_length(self.convert_to_number(value)?)?;
        self.update_array_copy_within_scalars(state, |pending| pending.length = length)?;
        let argument = self.array_copy_within_snapshot(state)?.target_argument;
        self.convert_array_copy_within_argument(
            site,
            state,
            ConversionConsumer::ArrayCopyWithinTarget,
            argument,
        )
    }

    /// Stores the normalized target and begins start conversion.
    fn finish_array_copy_within_target(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let relative = copy_within_integer(self.convert_to_number(value)?)?;
        let snapshot = self.array_copy_within_snapshot(state)?;
        let target = copy_within_relative_index(relative, snapshot.length);
        self.update_array_copy_within_scalars(state, |pending| pending.to = target)?;
        self.convert_array_copy_within_argument(
            site,
            state,
            ConversionConsumer::ArrayCopyWithinStart,
            snapshot.start_argument,
        )
    }

    /// Stores the normalized source start and begins optional end conversion.
    fn finish_array_copy_within_start(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let relative = copy_within_integer(self.convert_to_number(value)?)?;
        let snapshot = self.array_copy_within_snapshot(state)?;
        let from = copy_within_relative_index(relative, snapshot.length);
        self.update_array_copy_within_scalars(state, |pending| pending.from = from)?;
        if snapshot.end_argument.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_array_copy_within_bounds(site, state, snapshot.length);
        }
        self.convert_array_copy_within_argument(
            site,
            state,
            ConversionConsumer::ArrayCopyWithinEnd,
            snapshot.end_argument,
        )
    }

    /// Normalizes the explicit end and computes traversal direction.
    fn finish_array_copy_within_end(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let relative = copy_within_integer(self.convert_to_number(value)?)?;
        let length = self.array_copy_within_snapshot(state)?.length;
        let end = copy_within_relative_index(relative, length);
        self.finish_array_copy_within_bounds(site, state, end)
    }

    /// Computes count and adjusts cursors before the first observable indexed operation.
    fn finish_array_copy_within_bounds(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        end: u64,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_copy_within_snapshot(state)?;
        let count = end
            .saturating_sub(snapshot.from)
            .min(snapshot.length.saturating_sub(snapshot.to));
        let backwards = snapshot.from < snapshot.to && snapshot.to < snapshot.from + count;
        self.update_array_copy_within_scalars(state, |pending| {
            pending.count = count;
            if backwards && count != 0 {
                pending.direction = -1;
                pending.from += count - 1;
                pending.to += count - 1;
            }
        })?;
        self.advance_array_copy_within(site, state)
    }

    /// Runs immediately completed moves in a loop so source length cannot grow the Rust stack.
    fn advance_array_copy_within(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayCopyWithin>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_copy_within_snapshot(state)?;
            if snapshot.count == 0 {
                return self.write(site.caller_base, site.destination, snapshot.receiver);
            }
            let Some((current, present)) = self.dispatch_array_copy_within_has(
                site,
                state,
                ArrayCopyWithinStage::MoveHas,
                snapshot.receiver,
                safe_integer_value(snapshot.from),
            )?
            else {
                return Ok(());
            };
            if self.is_truthy_value(present)? {
                let key = self.safe_integer_property_atom(snapshot.from)?;
                let Some((current, value)) = self.dispatch_array_copy_within_get(
                    site,
                    current,
                    ArrayCopyWithinStage::MoveGet,
                    snapshot.receiver,
                    key.into(),
                )?
                else {
                    return Ok(());
                };
                self.set_array_copy_within_retained(current, value)?;
                let key = self.safe_integer_property_atom(snapshot.to)?;
                let Some(completed) = self.dispatch_array_copy_within_set(
                    site,
                    current,
                    ArrayCopyWithinStage::MoveSet,
                    snapshot.receiver,
                    key.into(),
                    value,
                )?
                else {
                    return Ok(());
                };
                state = completed;
            } else {
                let Some(completed) = self.dispatch_array_copy_within_delete(
                    site,
                    current,
                    ArrayCopyWithinStage::MoveDelete,
                    snapshot.receiver,
                    safe_integer_value(snapshot.to),
                )?
                else {
                    return Ok(());
                };
                state = completed;
            }
            self.commit_array_copy_within_move(state)?;
        }
    }

    /// Branches a suspended HasProperty completion into Get or Delete.
    fn finish_array_copy_within_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_copy_within_snapshot(state)?;
        if self.is_truthy_value(value)? {
            let key = self.safe_integer_property_atom(snapshot.from)?;
            if let Some((state, value)) = self.dispatch_array_copy_within_get(
                site,
                state,
                ArrayCopyWithinStage::MoveGet,
                snapshot.receiver,
                key.into(),
            )? {
                return self.finish_array_copy_within_get(site, state, value);
            }
            return Ok(());
        }
        if let Some(state) = self.dispatch_array_copy_within_delete(
            site,
            state,
            ArrayCopyWithinStage::MoveDelete,
            snapshot.receiver,
            safe_integer_value(snapshot.to),
        )? {
            return self.finish_array_copy_within_move(site, state);
        }
        Ok(())
    }

    /// Retains a suspended Get result while Set can invoke arbitrary JavaScript.
    fn finish_array_copy_within_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_copy_within_retained(state, value)?;
        let snapshot = self.array_copy_within_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.to)?;
        if let Some(state) = self.dispatch_array_copy_within_set(
            site,
            state,
            ArrayCopyWithinStage::MoveSet,
            snapshot.receiver,
            key.into(),
            value,
        )? {
            return self.finish_array_copy_within_move(site, state);
        }
        Ok(())
    }

    /// Commits one successful mutation and advances the remaining traversal.
    fn finish_array_copy_within_move(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
    ) -> Result<(), ExecutionError> {
        self.commit_array_copy_within_move(state)?;
        self.advance_array_copy_within(site, state)
    }

    /// Converts one target/start/end argument or dispatches its ToPrimitive callback.
    fn convert_array_copy_within_argument(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayCopyWithin>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_array_copy_within_conversion(site, state, consumer, value);
        }
        self.resume_array_copy_within_conversion(site, state, consumer, value)
    }
}

#[inline(always)]
fn copy_within_to_length(value: Value) -> Result<u64, ExecutionError> {
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
fn copy_within_integer(value: Value) -> Result<f64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
    })
}

#[inline(always)]
fn copy_within_relative_index(relative: f64, length: u64) -> u64 {
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
