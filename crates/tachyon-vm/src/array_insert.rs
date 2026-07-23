//! Resumable `Array.prototype.push` and `Array.prototype.unshift` algorithms.

use core::mem::size_of;

use super::*;

mod support;

/// GC-owned inputs and cursors shared by push and unshift.
#[derive(Debug)]
pub(crate) struct PendingArrayInsert {
    receiver: Value,
    retained: Value,
    items: Box<[Value]>,
    length: u64,
    cursor: u64,
    unshift: bool,
}

impl Trace for PendingArrayInsert {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.retained.trace(tracer);
        self.items.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayInsert {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.items.len().saturating_mul(size_of::<Value>())
    }
}

#[derive(Clone, Copy)]
struct ArrayInsertSnapshot {
    receiver: Value,
    length: u64,
    cursor: u64,
    item_count: u64,
    unshift: bool,
}

impl Isolate {
    /// Freezes arguments before beginning the observable length lookup.
    pub(crate) fn begin_array_insert(
        &mut self,
        site: &CallSite,
        unshift: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let item_count = site.argument_count as usize;
        let mut items = Vec::new();
        items
            .try_reserve_exact(item_count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        for index in 0..site.argument_count {
            items.push(
                self.call_argument(site, index)?
                    .ok_or(ExecutionError::RegisterAllocationFailed)?,
            );
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_array_insert_state(PendingArrayInsert {
            receiver,
            retained: undefined,
            items: items.into_boxed_slice(),
            length: 0,
            cursor: 0,
            unshift,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_insert_state(native_site, state)?;
        let length = self.length_atom()?;
        let observed = self.dispatch_array_insert_get(
            native_site,
            state,
            ArrayInsertStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some((state, value)) = observed {
            self.resume_array_insert(native_site, state, ArrayInsertStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes every observable completion into the explicit insertion stage machine.
    pub(crate) fn resume_array_insert(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        stage: ArrayInsertStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_insert_state(site, state)?;
        match stage {
            ArrayInsertStage::Length => self.resume_array_insert_length(site, state, value),
            ArrayInsertStage::MoveHas => self.finish_array_insert_move_has(site, state, value),
            ArrayInsertStage::MoveGet => self.finish_array_insert_move_get(site, state, value),
            ArrayInsertStage::MoveSet | ArrayInsertStage::MoveDelete => {
                self.finish_array_insert_move(site, state)
            }
            ArrayInsertStage::ItemSet => self.finish_array_insert_item(site, state),
            ArrayInsertStage::FinalLength => self.finish_array_insert(site, state),
        }
    }

    /// Resumes ToPrimitive(length) for one insertion operation.
    pub(crate) fn resume_array_insert_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_insert_state(site, state)?;
        self.finish_array_insert_length(site, state, value)
    }

    /// Enters object-to-primitive conversion when the observed length is an object.
    fn resume_array_insert_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayInsertLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_insert_length(site, state, value)
    }

    /// Applies ToLength, checks overflow, then selects movement or item insertion.
    fn finish_array_insert_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = array_insert_to_length(self.convert_to_number(value)?)?;
        let snapshot = self.array_insert_snapshot(state)?;
        if snapshot.item_count != 0
            && length
                .checked_add(snapshot.item_count)
                .is_none_or(|new_length| new_length > MAX_SAFE_INTEGER)
        {
            return Err(ExecutionError::ArrayLengthOverflow);
        }
        self.update_array_insert_scalars(state, |pending| {
            pending.length = length;
            pending.cursor = if pending.unshift && !pending.items.is_empty() {
                length
            } else {
                0
            };
        })?;
        if snapshot.unshift && snapshot.item_count != 0 && length != 0 {
            self.advance_array_insert_move(site, state)
        } else {
            self.advance_array_insert_items(site, state)
        }
    }

    /// Moves unshift sources from right to left, beginning each index with HasProperty.
    fn advance_array_insert_move(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayInsert>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_insert_snapshot(state)?;
            if snapshot.cursor == 0 {
                return self.advance_array_insert_items(site, state);
            }
            let from = snapshot.cursor - 1;
            let Some((current, present)) = self.dispatch_array_insert_has(
                site,
                state,
                ArrayInsertStage::MoveHas,
                snapshot.receiver,
                safe_integer_value(from),
            )?
            else {
                return Ok(());
            };
            if self.is_truthy_value(present)? {
                let key = self.safe_integer_property_atom(from)?;
                let Some((current, value)) = self.dispatch_array_insert_get(
                    site,
                    current,
                    ArrayInsertStage::MoveGet,
                    snapshot.receiver,
                    key.into(),
                )?
                else {
                    return Ok(());
                };
                self.set_array_insert_retained(current, value)?;
                let to = from + snapshot.item_count;
                let key = self.safe_integer_property_atom(to)?;
                let Some(completed) = self.dispatch_array_insert_set(
                    site,
                    current,
                    ArrayInsertStage::MoveSet,
                    snapshot.receiver,
                    key.into(),
                    value,
                )?
                else {
                    return Ok(());
                };
                state = completed;
            } else {
                let Some(completed) = self.dispatch_array_insert_delete(
                    site,
                    current,
                    ArrayInsertStage::MoveDelete,
                    snapshot.receiver,
                    safe_integer_value(from + snapshot.item_count),
                )?
                else {
                    return Ok(());
                };
                state = completed;
            }
            self.update_array_insert_scalars(state, |pending| pending.cursor -= 1)?;
        }
    }

    /// Branches a move presence result into source Get or destination Delete.
    fn finish_array_insert_move_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_insert_snapshot(state)?;
        let from = snapshot.cursor - 1;
        if self.is_truthy_value(value)? {
            let key = self.safe_integer_property_atom(from)?;
            let observed = self.dispatch_array_insert_get(
                site,
                state,
                ArrayInsertStage::MoveGet,
                snapshot.receiver,
                key.into(),
            )?;
            if let Some((state, value)) = observed {
                self.finish_array_insert_move_get(site, state, value)?;
            }
            return Ok(());
        }
        if let Some(state) = self.dispatch_array_insert_delete(
            site,
            state,
            ArrayInsertStage::MoveDelete,
            snapshot.receiver,
            safe_integer_value(from + snapshot.item_count),
        )? {
            return self.finish_array_insert_move(site, state);
        }
        Ok(())
    }

    /// Retains a moved source while Set(destination, source, true) can execute JavaScript.
    fn finish_array_insert_move_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_insert_retained(state, value)?;
        let snapshot = self.array_insert_snapshot(state)?;
        let to = snapshot.cursor - 1 + snapshot.item_count;
        let key = self.safe_integer_property_atom(to)?;
        if let Some(state) = self.dispatch_array_insert_set(
            site,
            state,
            ArrayInsertStage::MoveSet,
            snapshot.receiver,
            key.into(),
            value,
        )? {
            return self.finish_array_insert_move(site, state);
        }
        Ok(())
    }

    /// Decrements the backwards cursor only after Set/Delete succeeds.
    fn finish_array_insert_move(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
    ) -> Result<(), ExecutionError> {
        self.update_array_insert_scalars(state, |pending| pending.cursor -= 1)?;
        self.advance_array_insert_move(site, state)
    }

    /// Sets captured items from left to right and then performs the final length Set.
    fn advance_array_insert_items(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayInsert>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_insert_snapshot(state)?;
            if snapshot.cursor >= snapshot.item_count {
                let new_length = snapshot.length + snapshot.item_count;
                let length = self.length_atom()?;
                if let Some(completed) = self.dispatch_array_insert_set(
                    site,
                    state,
                    ArrayInsertStage::FinalLength,
                    snapshot.receiver,
                    length.into(),
                    safe_integer_value(new_length),
                )? {
                    return self.finish_array_insert(site, completed);
                }
                return Ok(());
            }
            let item = self.array_insert_item(state, snapshot.cursor as usize)?;
            let index = if snapshot.unshift {
                snapshot.cursor
            } else {
                snapshot.length + snapshot.cursor
            };
            let key = self.safe_integer_property_atom(index)?;
            let Some(completed) = self.dispatch_array_insert_set(
                site,
                state,
                ArrayInsertStage::ItemSet,
                snapshot.receiver,
                key.into(),
                item,
            )?
            else {
                return Ok(());
            };
            state = completed;
            self.update_array_insert_scalars(state, |pending| pending.cursor += 1)?;
        }
    }

    /// Advances the item cursor only after its Set succeeds.
    fn finish_array_insert_item(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
    ) -> Result<(), ExecutionError> {
        self.update_array_insert_scalars(state, |pending| pending.cursor += 1)?;
        self.advance_array_insert_items(site, state)
    }

    /// Returns the new safe-integer length after the final observable Set.
    fn finish_array_insert(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayInsert>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_insert_snapshot(state)?;
        self.write(
            site.caller_base,
            site.destination,
            safe_integer_value(snapshot.length + snapshot.item_count),
        )
    }
}

/// Applies ToLength to an already numeric primitive.
#[inline(always)]
fn array_insert_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if number.is_infinite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}
