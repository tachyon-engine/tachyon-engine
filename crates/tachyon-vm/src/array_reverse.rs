//! Resumable `Array.prototype.reverse` algorithm.

use super::*;

mod support;

/// GC-owned pair state across observable reverse operations.
#[derive(Debug)]
pub(crate) struct PendingArrayReverse {
    receiver: Value,
    lower_value: Value,
    upper_value: Value,
    length: u64,
    lower: u64,
    lower_present: bool,
    upper_present: bool,
}

impl Trace for PendingArrayReverse {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.lower_value.trace(tracer);
        self.upper_value.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayReverse {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

#[derive(Clone, Copy)]
struct ArrayReverseSnapshot {
    receiver: Value,
    lower_value: Value,
    upper_value: Value,
    length: u64,
    lower: u64,
    lower_present: bool,
    upper_present: bool,
}

impl Isolate {
    /// Captures the boxed receiver and begins the observable length lookup.
    pub(crate) fn begin_array_reverse(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_array_reverse_state(PendingArrayReverse {
            receiver,
            lower_value: undefined,
            upper_value: undefined,
            length: 0,
            lower: 0,
            lower_present: false,
            upper_present: false,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_reverse_state(native_site, state)?;
        let length = self.length_atom()?;
        let observed = self.dispatch_array_reverse_get(
            native_site,
            state,
            ArrayReverseStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some((state, value)) = observed {
            self.resume_array_reverse(native_site, state, ArrayReverseStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes each observable completion into the reverse pair state machine.
    pub(crate) fn resume_array_reverse(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        stage: ArrayReverseStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_reverse_state(site, state)?;
        match stage {
            ArrayReverseStage::Length => self.resume_array_reverse_length(site, state, value),
            ArrayReverseStage::LowerHas => self.finish_array_reverse_lower_has(site, state, value),
            ArrayReverseStage::LowerGet => self.finish_array_reverse_lower_get(site, state, value),
            ArrayReverseStage::UpperHas => self.finish_array_reverse_upper_has(site, state, value),
            ArrayReverseStage::UpperGet => self.finish_array_reverse_upper_get(site, state, value),
            ArrayReverseStage::FirstMutation => {
                self.finish_array_reverse_first_mutation(site, state)
            }
            ArrayReverseStage::SecondMutation => self.finish_array_reverse_pair(site, state),
        }
    }

    /// Resumes ToPrimitive(length) for one reverse operation.
    pub(crate) fn resume_array_reverse_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_reverse_state(site, state)?;
        self.finish_array_reverse_length(site, state, value)
    }

    /// Enters object-to-primitive conversion when length is an object.
    fn resume_array_reverse_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayReverseLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_reverse_length(site, state, value)
    }

    /// Stores ToLength before beginning the first lower/upper pair.
    fn finish_array_reverse_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = array_reverse_to_length(self.convert_to_number(value)?)?;
        self.update_array_reverse_scalars(state, |pending| pending.length = length)?;
        self.advance_array_reverse(site, state)
    }

    /// Runs synchronously completed pairs in a loop so length cannot grow the Rust stack.
    fn advance_array_reverse(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_reverse_snapshot(state)?;
            if snapshot.lower >= snapshot.length / 2 {
                return self.write(site.caller_base, site.destination, snapshot.receiver);
            }
            self.reset_array_reverse_pair(state)?;
            let Some((current, lower_present)) = self.dispatch_array_reverse_has(
                site,
                state,
                ArrayReverseStage::LowerHas,
                snapshot.receiver,
                safe_integer_value(snapshot.lower),
            )?
            else {
                return Ok(());
            };
            state = current;
            let lower_present = self.is_truthy_value(lower_present)?;
            self.set_array_reverse_presence(state, true, lower_present)?;
            if lower_present {
                let key = self.safe_integer_property_atom(snapshot.lower)?;
                let Some((current, value)) = self.dispatch_array_reverse_get(
                    site,
                    state,
                    ArrayReverseStage::LowerGet,
                    snapshot.receiver,
                    key.into(),
                )?
                else {
                    return Ok(());
                };
                state = current;
                self.set_array_reverse_value(state, true, value)?;
            }
            let upper = snapshot.length - snapshot.lower - 1;
            let Some((current, upper_present)) = self.dispatch_array_reverse_has(
                site,
                state,
                ArrayReverseStage::UpperHas,
                snapshot.receiver,
                safe_integer_value(upper),
            )?
            else {
                return Ok(());
            };
            state = current;
            let upper_present = self.is_truthy_value(upper_present)?;
            self.set_array_reverse_presence(state, false, upper_present)?;
            if upper_present {
                let key = self.safe_integer_property_atom(upper)?;
                let Some((current, value)) = self.dispatch_array_reverse_get(
                    site,
                    state,
                    ArrayReverseStage::UpperGet,
                    snapshot.receiver,
                    key.into(),
                )?
                else {
                    return Ok(());
                };
                state = current;
                self.set_array_reverse_value(state, false, value)?;
            }
            let Some(completed) = self.dispatch_array_reverse_mutations(site, state)? else {
                return Ok(());
            };
            state = completed;
            self.commit_array_reverse_pair(state)?;
        }
    }

    /// Continues after a suspended lower HasProperty result.
    fn finish_array_reverse_lower_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let present = self.is_truthy_value(value)?;
        self.set_array_reverse_presence(state, true, present)?;
        if present {
            let snapshot = self.array_reverse_snapshot(state)?;
            let key = self.safe_integer_property_atom(snapshot.lower)?;
            if let Some((state, value)) = self.dispatch_array_reverse_get(
                site,
                state,
                ArrayReverseStage::LowerGet,
                snapshot.receiver,
                key.into(),
            )? {
                return self.finish_array_reverse_lower_get(site, state, value);
            }
            return Ok(());
        }
        self.begin_array_reverse_upper(site, state)
    }

    /// Retains the lower value before observing the upper side.
    fn finish_array_reverse_lower_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_reverse_value(state, true, value)?;
        self.begin_array_reverse_upper(site, state)
    }

    /// Begins the upper HasProperty operation after lower observation is complete.
    fn begin_array_reverse_upper(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_reverse_snapshot(state)?;
        let upper = snapshot.length - snapshot.lower - 1;
        if let Some((state, value)) = self.dispatch_array_reverse_has(
            site,
            state,
            ArrayReverseStage::UpperHas,
            snapshot.receiver,
            safe_integer_value(upper),
        )? {
            return self.finish_array_reverse_upper_has(site, state, value);
        }
        Ok(())
    }

    /// Continues an upper presence result into optional Get or pair mutation.
    fn finish_array_reverse_upper_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let present = self.is_truthy_value(value)?;
        self.set_array_reverse_presence(state, false, present)?;
        if present {
            let snapshot = self.array_reverse_snapshot(state)?;
            let upper = snapshot.length - snapshot.lower - 1;
            let key = self.safe_integer_property_atom(upper)?;
            if let Some((state, value)) = self.dispatch_array_reverse_get(
                site,
                state,
                ArrayReverseStage::UpperGet,
                snapshot.receiver,
                key.into(),
            )? {
                return self.finish_array_reverse_upper_get(site, state, value);
            }
            return Ok(());
        }
        self.apply_array_reverse_pair(site, state)
    }

    /// Retains the upper value before applying the pair's mutation branch.
    fn finish_array_reverse_upper_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_reverse_value(state, false, value)?;
        self.apply_array_reverse_pair(site, state)
    }

    /// Applies both pair mutations immediately when possible, otherwise suspends.
    fn apply_array_reverse_pair(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        if let Some(state) = self.dispatch_array_reverse_mutations(site, state)? {
            return self.finish_array_reverse_pair(site, state);
        }
        Ok(())
    }

    /// Dispatches the specification's first mutation and then the second mutation.
    fn dispatch_array_reverse_mutations(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<Option<GcRef<PendingArrayReverse>>, ExecutionError> {
        let snapshot = self.array_reverse_snapshot(state)?;
        if !snapshot.lower_present && !snapshot.upper_present {
            return Ok(Some(state));
        }
        let first = if snapshot.upper_present {
            let key = self.safe_integer_property_atom(snapshot.lower)?;
            self.dispatch_array_reverse_set(
                site,
                state,
                ArrayReverseStage::FirstMutation,
                snapshot.receiver,
                key.into(),
                snapshot.upper_value,
            )?
        } else {
            self.dispatch_array_reverse_delete(
                site,
                state,
                ArrayReverseStage::FirstMutation,
                snapshot.receiver,
                safe_integer_value(snapshot.lower),
            )?
        };
        let Some(state) = first else {
            return Ok(None);
        };
        self.dispatch_array_reverse_second_mutation(site, state)
    }

    /// Dispatches the second mutation selected by lower-side presence.
    fn dispatch_array_reverse_second_mutation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<Option<GcRef<PendingArrayReverse>>, ExecutionError> {
        let snapshot = self.array_reverse_snapshot(state)?;
        let upper = snapshot.length - snapshot.lower - 1;
        if snapshot.lower_present {
            let key = self.safe_integer_property_atom(upper)?;
            self.dispatch_array_reverse_set(
                site,
                state,
                ArrayReverseStage::SecondMutation,
                snapshot.receiver,
                key.into(),
                snapshot.lower_value,
            )
        } else {
            self.dispatch_array_reverse_delete(
                site,
                state,
                ArrayReverseStage::SecondMutation,
                snapshot.receiver,
                safe_integer_value(upper),
            )
        }
    }

    /// Continues after the first mutation, committing nothing until the second succeeds.
    fn finish_array_reverse_first_mutation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        if let Some(state) = self.dispatch_array_reverse_second_mutation(site, state)? {
            return self.finish_array_reverse_pair(site, state);
        }
        Ok(())
    }

    /// Commits one fully mutated pair and returns to the explicit loop.
    fn finish_array_reverse_pair(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayReverse>,
    ) -> Result<(), ExecutionError> {
        self.commit_array_reverse_pair(state)?;
        self.advance_array_reverse(site, state)
    }
}

/// Applies ToLength to an already numeric primitive.
#[inline(always)]
fn array_reverse_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}
