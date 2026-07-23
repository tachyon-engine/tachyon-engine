//! Resumable `Array.prototype.pop` and `Array.prototype.shift` algorithms.

use super::*;

/// GC-owned fixed state shared by pop and shift across observable JavaScript work.
#[derive(Debug)]
pub(crate) struct PendingArrayRemove {
    receiver: Value,
    retained: Value,
    length: u64,
    cursor: u64,
    shift: bool,
}

impl Trace for PendingArrayRemove {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.retained.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayRemove {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

#[derive(Clone, Copy)]
struct ArrayRemoveSnapshot {
    receiver: Value,
    retained: Value,
    length: u64,
    cursor: u64,
    shift: bool,
}

impl Isolate {
    /// Captures the receiver and begins the observable LengthOfArrayLike operation.
    pub(crate) fn begin_array_remove(
        &mut self,
        site: &CallSite,
        shift: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_array_remove_state(PendingArrayRemove {
            receiver,
            retained: undefined,
            length: 0,
            cursor: 0,
            shift,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_remove_state(native_site, state)?;
        let length = self.length_atom()?;
        let observed = self.dispatch_array_remove_get(
            native_site,
            state,
            ArrayRemoveStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some((state, value)) = observed {
            self.resume_array_remove(native_site, state, ArrayRemoveStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes each completed Get, Has, Set, or Delete into the next algorithm stage.
    pub(crate) fn resume_array_remove(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        stage: ArrayRemoveStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_remove_state(site, state)?;
        match stage {
            ArrayRemoveStage::Length => self.resume_array_remove_length(site, state, value),
            ArrayRemoveStage::ElementGet => self.finish_array_remove_element(site, state, value),
            ArrayRemoveStage::SourceHas => self.finish_array_remove_has(site, state, value),
            ArrayRemoveStage::SourceGet => self.finish_array_remove_get(site, state, value),
            ArrayRemoveStage::TargetSet | ArrayRemoveStage::TargetDelete => {
                self.finish_array_remove_move(site, state)
            }
            ArrayRemoveStage::TailDelete => self.set_array_remove_length(site, state),
            ArrayRemoveStage::FinalLength => self.finish_array_remove(site, state),
        }
    }

    /// Resumes ToPrimitive(length) without allowing the state to escape the GC root set.
    pub(crate) fn resume_array_remove_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_remove_state(site, state)?;
        self.finish_array_remove_length(site, state, value)
    }

    /// Dispatches object conversion when required, otherwise applies ToLength immediately.
    fn resume_array_remove_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayRemoveLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_remove_length(site, state, value)
    }

    /// Stores ToLength and selects the zero-length Set or first element Get.
    fn finish_array_remove_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = array_remove_to_length(self.convert_to_number(value)?)?;
        self.update_array_remove_scalars(state, |pending| pending.length = length)?;
        if length == 0 {
            return self.set_array_remove_length(site, state);
        }
        let snapshot = self.array_remove_snapshot(state)?;
        let index = if snapshot.shift { 0 } else { length - 1 };
        let key = self.safe_integer_property_atom(index)?;
        let observed = self.dispatch_array_remove_get(
            site,
            state,
            ArrayRemoveStage::ElementGet,
            snapshot.receiver,
            key.into(),
        )?;
        if let Some((state, value)) = observed {
            self.finish_array_remove_element(site, state, value)?;
        }
        Ok(())
    }

    /// Retains the result across mutations and begins shifting or tail deletion.
    fn finish_array_remove_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_remove_retained(state, value)?;
        let snapshot = self.array_remove_snapshot(state)?;
        if snapshot.shift {
            self.update_array_remove_scalars(state, |pending| pending.cursor = 1)?;
            self.advance_array_remove_move(site, state)
        } else {
            self.delete_array_remove_tail(site, state)
        }
    }

    /// Advances shift left-to-right, preserving one HasProperty per source index.
    fn advance_array_remove_move(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_remove_snapshot(state)?;
        if snapshot.cursor >= snapshot.length {
            return self.delete_array_remove_tail(site, state);
        }
        let observed = self.dispatch_array_remove_has(
            site,
            state,
            ArrayRemoveStage::SourceHas,
            snapshot.receiver,
            safe_integer_value(snapshot.cursor),
        )?;
        if let Some((state, value)) = observed {
            self.finish_array_remove_has(site, state, value)?;
        }
        Ok(())
    }

    /// Branches a source presence result into Get(source) or Delete(target).
    fn finish_array_remove_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_remove_snapshot(state)?;
        if self.is_truthy_value(value)? {
            let key = self.safe_integer_property_atom(snapshot.cursor)?;
            let observed = self.dispatch_array_remove_get(
                site,
                state,
                ArrayRemoveStage::SourceGet,
                snapshot.receiver,
                key.into(),
            )?;
            if let Some((state, value)) = observed {
                self.finish_array_remove_get(site, state, value)?;
            }
            return Ok(());
        }
        self.dispatch_array_remove_delete(
            site,
            state,
            ArrayRemoveStage::TargetDelete,
            snapshot.receiver,
            safe_integer_value(snapshot.cursor - 1),
        )
    }

    /// Performs Set(target, sourceValue, true) for one present shift source.
    fn finish_array_remove_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_remove_snapshot(state)?;
        let target = self.safe_integer_property_atom(snapshot.cursor - 1)?;
        self.dispatch_array_remove_set(
            site,
            state,
            ArrayRemoveStage::TargetSet,
            snapshot.receiver,
            target.into(),
            value,
        )
    }

    /// Commits the source cursor only after the target mutation succeeds.
    fn finish_array_remove_move(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
    ) -> Result<(), ExecutionError> {
        self.update_array_remove_scalars(state, |pending| pending.cursor += 1)?;
        self.advance_array_remove_move(site, state)
    }

    /// Deletes the final indexed property for both pop and shift.
    fn delete_array_remove_tail(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_remove_snapshot(state)?;
        self.dispatch_array_remove_delete(
            site,
            state,
            ArrayRemoveStage::TailDelete,
            snapshot.receiver,
            safe_integer_value(snapshot.length - 1),
        )
    }

    /// Always performs the observable final Set(O, "length", newLength, true).
    fn set_array_remove_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_remove_snapshot(state)?;
        let new_length = snapshot.length.saturating_sub(1);
        let length = self.length_atom()?;
        self.dispatch_array_remove_set(
            site,
            state,
            ArrayRemoveStage::FinalLength,
            snapshot.receiver,
            length.into(),
            safe_integer_value(new_length),
        )
    }

    /// Publishes the retained result only after all required mutations succeed.
    fn finish_array_remove(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayRemove>,
    ) -> Result<(), ExecutionError> {
        let result = self.array_remove_snapshot(state)?.retained;
        self.write(site.caller_base, site.destination, result)
    }
}

mod support;

/// Applies ToLength to an already numeric primitive.
#[inline(always)]
fn array_remove_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if number.is_infinite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}
