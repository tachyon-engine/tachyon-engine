//! Resumable `Array.prototype.splice` species, copy, and mutation algorithm.

use core::mem::size_of;

use super::*;

mod support;
use support::{relative_start, splice_integer, splice_move_indices, splice_to_length};

/// GC-owned splice inputs and scalar cursor state across observable JavaScript work.
#[derive(Debug)]
pub(crate) struct PendingArraySplice {
    receiver: Value,
    result: Value,
    retained: Value,
    constructor: Value,
    start_argument: Value,
    delete_argument: Value,
    items: Box<[Value]>,
    len: u64,
    start: u64,
    delete_count: u64,
    new_len: u64,
    cursor: u64,
    argument_count: u32,
}

impl Trace for PendingArraySplice {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.result.trace(tracer);
        self.retained.trace(tracer);
        self.constructor.trace(tracer);
        self.start_argument.trace(tracer);
        self.delete_argument.trace(tracer);
        self.items.trace(tracer);
    }
}

impl GcExternalMemory for PendingArraySplice {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.items.len().saturating_mul(size_of::<Value>())
    }
}

#[derive(Clone, Copy)]
struct ArraySpliceSnapshot {
    receiver: Value,
    result: Value,
    len: u64,
    start: u64,
    delete_count: u64,
    new_len: u64,
    cursor: u64,
    argument_count: u32,
    item_count: u64,
}

impl Isolate {
    /// Captures every splice argument before beginning the observable length lookup.
    pub(crate) fn begin_array_splice(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let start_argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let delete_argument = self.call_argument(site, 1)?.unwrap_or(undefined);
        let item_count = site.argument_count.saturating_sub(2) as usize;
        let mut items = Vec::new();
        items
            .try_reserve_exact(item_count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        for index in 0..item_count {
            items.push(
                self.call_argument(site, index as u32 + 2)?
                    .ok_or(ExecutionError::RegisterAllocationFailed)?,
            );
        }
        let state = self.allocate_array_splice_state(PendingArraySplice {
            receiver,
            result: undefined,
            retained: undefined,
            constructor: undefined,
            start_argument,
            delete_argument,
            items: items.into_boxed_slice(),
            len: 0,
            start: 0,
            delete_count: 0,
            new_len: 0,
            cursor: 0,
            argument_count: site.argument_count,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_splice_state(native_site, state)?;
        let length = self.length_atom()?;
        let value = self.dispatch_array_splice_get(
            native_site,
            state,
            ArraySpliceStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some((state, value)) = value {
            self.resume_array_splice(
                native_site,
                state,
                ArraySpliceStage::Length,
                value,
                receiver,
            )?;
        }
        Ok(())
    }

    /// Routes every observable splice completion back into its explicit algorithm stage.
    pub(crate) fn resume_array_splice(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        stage: ArraySpliceStage,
        value: Value,
        _retained: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_splice_state(site, state)?;
        match stage {
            ArraySpliceStage::Length => self.resume_array_splice_length(site, state, value),
            ArraySpliceStage::SpeciesConstructor => {
                self.resume_array_splice_constructor(site, state, value)
            }
            ArraySpliceStage::SpeciesValue => {
                self.finish_array_splice_species(site, state, value, true)
            }
            ArraySpliceStage::SpeciesConstruct => {
                self.finish_array_splice_construct(site, state, value)
            }
            ArraySpliceStage::CopyHas => self.resume_array_splice_copy_has(site, state, value),
            ArraySpliceStage::CopyGet => self.resume_array_splice_copy_get(site, state, value),
            ArraySpliceStage::CopyDefine => self.finish_array_splice_copy(site, state),
            ArraySpliceStage::ResultLength => self.begin_array_splice_mutation(site, state),
            ArraySpliceStage::MoveHas => self.resume_array_splice_move_has(site, state, value),
            ArraySpliceStage::MoveGet => self.resume_array_splice_move_get(site, state, value),
            ArraySpliceStage::MoveSet | ArraySpliceStage::MoveDelete => {
                self.finish_array_splice_move(site, state)
            }
            ArraySpliceStage::TailDelete => self.finish_array_splice_tail(site, state),
            ArraySpliceStage::InsertSet => self.finish_array_splice_insert(site, state),
            ArraySpliceStage::FinalLength => self.finish_array_splice(site, state),
        }
    }

    /// Continues object-to-primitive conversion for length, start, or deleteCount.
    pub(crate) fn resume_array_splice_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_splice_state(site, state)?;
        match consumer {
            ConversionConsumer::ArraySpliceLength => {
                self.finish_array_splice_length_primitive(site, state, value)
            }
            ConversionConsumer::ArraySpliceStart => {
                self.finish_array_splice_start_primitive(site, state, value)
            }
            ConversionConsumer::ArraySpliceDeleteCount => {
                self.finish_array_splice_delete_primitive(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Applies ToLength, then begins the required start conversion even for zero arguments.
    fn resume_array_splice_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArraySpliceLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_splice_length_primitive(site, state, value)
    }

    /// Stores ToLength and dispatches ToIntegerOrInfinity(start).
    fn finish_array_splice_length_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = splice_to_length(self.convert_to_number(value)?)?;
        self.update_array_splice_scalars(state, |pending| pending.len = length)?;
        let start = self
            .array_splice_snapshot(state)?
            .start_argument(self, state)?;
        if self.is_object_value(start) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArraySpliceStart,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                start,
                site.call_site,
            );
        }
        self.finish_array_splice_start_primitive(site, state, start)
    }

    /// Clamps relative start and branches by the actual argument count.
    fn finish_array_splice_start_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let snapshot = self.array_splice_snapshot(state)?;
        let start = relative_start(snapshot.len, splice_integer(number));
        self.update_array_splice_scalars(state, |pending| pending.start = start)?;
        if snapshot.argument_count == 0 {
            return self.finish_array_splice_arguments(site, state, 0);
        }
        if snapshot.argument_count == 1 {
            return self.finish_array_splice_arguments(site, state, snapshot.len - start);
        }
        let delete = self.array_splice_delete_argument(state)?;
        if self.is_object_value(delete) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArraySpliceDeleteCount,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                delete,
                site.call_site,
            );
        }
        self.finish_array_splice_delete_primitive(site, state, delete)
    }

    /// Converts and clamps an explicitly supplied deleteCount.
    fn finish_array_splice_delete_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let snapshot = self.array_splice_snapshot(state)?;
        let available = snapshot.len - snapshot.start;
        let integer = splice_integer(number);
        let delete_count = if integer <= 0.0 {
            0
        } else if integer >= available as f64 {
            available
        } else {
            integer as u64
        };
        self.finish_array_splice_arguments(site, state, delete_count)
    }

    /// Freezes computed counts, rejects overflow, and begins ArraySpeciesCreate.
    fn finish_array_splice_arguments(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        delete_count: u64,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let new_len = snapshot
            .len
            .checked_sub(delete_count)
            .and_then(|base| base.checked_add(snapshot.item_count))
            .filter(|length| *length <= MAX_SAFE_INTEGER)
            .ok_or(ExecutionError::ArrayLengthOverflow)?;
        self.update_array_splice_scalars(state, |pending| {
            pending.delete_count = delete_count;
            pending.new_len = new_len;
            pending.cursor = 0;
        })?;
        self.begin_array_splice_species(site, state)
    }

    /// Implements the initial IsArray and observable constructor lookup of ArraySpeciesCreate.
    fn begin_array_splice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.array_splice_snapshot(state)?.receiver;
        if !self.is_array_value(receiver)? {
            return self.finish_array_splice_species(
                site,
                state,
                Value::from_immediate(Immediate::Undefined),
                false,
            );
        }
        let constructor = self.constructor_atom()?;
        let value = self.dispatch_array_splice_get(
            site,
            state,
            ArraySpliceStage::SpeciesConstructor,
            receiver,
            constructor.into(),
        )?;
        if let Some((state, value)) = value {
            self.resume_array_splice_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Applies cross-Realm intrinsic Array fallback before reading Symbol.species.
    fn resume_array_splice_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return self.finish_array_splice_species(site, state, value, false);
        }
        if self.is_constructor_value(value)? {
            let constructor_realm = self.realm_for_callable(value)?;
            if constructor_realm != self.active_realm
                && self.realm_array_constructor(constructor_realm) == Some(value)
            {
                return self.finish_array_splice_species(
                    site,
                    state,
                    Value::from_immediate(Immediate::Undefined),
                    false,
                );
            }
        }
        self.set_array_splice_value(state, |pending| &mut pending.constructor, value)?;
        let species = self
            .realm
            .well_known_symbols
            .species
            .expect("Symbol.species initializes before Array");
        let key = self.property_key(species)?;
        let observed = self.dispatch_array_splice_get(
            site,
            state,
            ArraySpliceStage::SpeciesValue,
            value,
            key,
        )?;
        if let Some((state, observed)) = observed {
            self.finish_array_splice_species(site, state, observed, true)?;
        }
        Ok(())
    }

    /// Creates an intrinsic Array or invokes the selected custom species constructor.
    fn finish_array_splice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        constructor: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined)
            || (from_species && constructor.as_immediate() == Some(Immediate::Null))
        {
            self.root_array_splice_state(site, state)?;
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before splice");
            let result = self.create_array_object_with_prototype(prototype)?;
            let state = self
                .pending_array_splice_reference(self.read(site.caller_base, site.destination)?)?;
            self.set_array_splice_value(state, |pending| &mut pending.result, result)?;
            let count = self.array_splice_snapshot(state)?.delete_count;
            self.set_array_length_value(result, safe_integer_value(count))?;
            return self.advance_array_splice_copy(site, state);
        }
        if constructor.as_immediate() == Some(Immediate::Null) || !self.is_object_value(constructor)
        {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_array_splice_species(site, state, constructor)
    }

    /// Roots the one-element argument prefix while a custom species constructor runs.
    fn construct_array_splice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_splice_value(state, |pending| &mut pending.constructor, constructor)?;
        self.push_array_splice_parent(
            site,
            state,
            ArraySpliceStage::SpeciesConstruct,
            constructor,
        )?;
        let count = self.array_splice_snapshot(state)?.delete_count;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let prefix = match self.create_apply_argument_prefix(
            constructor,
            undefined,
            vec![safe_integer_value(count)],
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_splice_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_array_splice_parent(
            site,
            state,
            ArraySpliceStage::SpeciesConstruct,
            Value::from_heap_ref(prefix.raw()),
        )?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.construct_site(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: constructor,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 1,
            argument_count: 1,
            this_value: undefined,
            new_target: constructor,
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
                .expect("Array species constructor publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_splice_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_splice_construct(site, state, result)
    }

    /// Validates and publishes the species result before copying deleted elements.
    fn finish_array_splice_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        self.set_array_splice_value(state, |pending| &mut pending.result, result)?;
        self.advance_array_splice_copy(site, state)
    }

    /// Copies present deleted properties with HasProperty/Get/CreateDataPropertyOrThrow ordering.
    fn advance_array_splice_copy(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.root_array_splice_state(site, state)?;
            let snapshot = self.array_splice_snapshot(state)?;
            if snapshot.cursor >= snapshot.delete_count {
                let length = self.length_atom()?;
                return self.dispatch_array_splice_set(
                    site,
                    state,
                    ArraySpliceStage::ResultLength,
                    snapshot.result,
                    length.into(),
                    safe_integer_value(snapshot.delete_count),
                );
            }
            let from = snapshot.start + snapshot.cursor;
            let Some((state, has)) = self.dispatch_array_splice_has(
                site,
                state,
                ArraySpliceStage::CopyHas,
                snapshot.receiver,
                safe_integer_value(from),
            )?
            else {
                return Ok(());
            };
            if self.is_truthy_value(has)? {
                return self.dispatch_array_splice_copy_get(site, state, from);
            }
            self.skip_array_splice_copy_holes(state, from)?;
        }
    }

    /// Handles a copied element's HasProperty result without treating holes as undefined.
    fn resume_array_splice_copy_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let from = snapshot.start + snapshot.cursor;
        if self.is_truthy_value(value)? {
            return self.dispatch_array_splice_copy_get(site, state, from);
        }
        self.skip_array_splice_copy_holes(state, from)?;
        self.advance_array_splice_copy(site, state)
    }

    /// Reads a present deleted property at the current source cursor.
    fn dispatch_array_splice_copy_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        from: u64,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let key = self.safe_integer_property_atom(from)?;
        let value = self.dispatch_array_splice_get(
            site,
            state,
            ArraySpliceStage::CopyGet,
            snapshot.receiver,
            key.into(),
        )?;
        if let Some((state, value)) = value {
            self.resume_array_splice_copy_get(site, state, value)?;
        }
        Ok(())
    }

    /// Defines one copied value on the species result with all data attributes true.
    fn resume_array_splice_copy_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_splice_state(site, state)?;
        self.set_array_splice_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_splice_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.cursor)?;
        let descriptor = DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        if self.is_proxy_value(snapshot.result) {
            return self.dispatch_array_splice_define(
                site,
                state,
                ArraySpliceStage::CopyDefine,
                snapshot.result,
                key.into(),
                descriptor.into(),
            );
        }
        self.define_data_property(snapshot.result, key, descriptor)?;
        let state =
            self.pending_array_splice_reference(self.read(site.caller_base, site.destination)?)?;
        self.finish_array_splice_copy(site, state)
    }

    /// Advances the deleted-result cursor only after a successful define.
    fn finish_array_splice_copy(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.update_array_splice_scalars(state, |pending| pending.cursor += 1)?;
        self.advance_array_splice_copy(site, state)
    }

    /// Selects forward shrink, backward growth, or direct insertion after result length Set.
    fn begin_array_splice_mutation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let cursor = if snapshot.item_count < snapshot.delete_count {
            snapshot.start
        } else if snapshot.item_count > snapshot.delete_count {
            snapshot.len - snapshot.delete_count
        } else {
            0
        };
        self.update_array_splice_scalars(state, |pending| pending.cursor = cursor)?;
        if snapshot.item_count == snapshot.delete_count {
            return self.begin_array_splice_insert(site, state);
        }
        self.advance_array_splice_move(site, state)
    }

    /// Advances the direction-sensitive move loop, suspending at HasProperty when required.
    fn advance_array_splice_move(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.root_array_splice_state(site, state)?;
        let snapshot = self.array_splice_snapshot(state)?;
        if snapshot.item_count < snapshot.delete_count {
            if snapshot.cursor >= snapshot.len - snapshot.delete_count {
                self.update_array_splice_scalars(state, |pending| pending.cursor = pending.len)?;
                return self.advance_array_splice_tail_delete(site, state);
            }
        } else if snapshot.cursor <= snapshot.start {
            return self.begin_array_splice_insert(site, state);
        }
        let (from, _) = splice_move_indices(snapshot);
        let Some((state, has)) = self.dispatch_array_splice_has(
            site,
            state,
            ArraySpliceStage::MoveHas,
            snapshot.receiver,
            safe_integer_value(from),
        )?
        else {
            return Ok(());
        };
        if self.is_truthy_value(has)? {
            return self.dispatch_array_splice_move_get(site, state, from);
        }
        self.dispatch_array_splice_move_delete(site, state)
    }

    /// Branches a move HasProperty result into Get/Set or Delete(to).
    fn resume_array_splice_move_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let (from, _) = splice_move_indices(snapshot);
        if self.is_truthy_value(value)? {
            self.dispatch_array_splice_move_get(site, state, from)
        } else {
            self.dispatch_array_splice_move_delete(site, state)
        }
    }

    /// Gets one known-present source property before setting its destination.
    fn dispatch_array_splice_move_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        from: u64,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let key = self.safe_integer_property_atom(from)?;
        let value = self.dispatch_array_splice_get(
            site,
            state,
            ArraySpliceStage::MoveGet,
            snapshot.receiver,
            key.into(),
        )?;
        if let Some((state, value)) = value {
            self.resume_array_splice_move_get(site, state, value)?;
        }
        Ok(())
    }

    /// Retains a moved value and performs Set(O, to, value, true).
    fn resume_array_splice_move_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_splice_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_splice_snapshot(state)?;
        let (_, to) = splice_move_indices(snapshot);
        let key = self.safe_integer_property_atom(to)?;
        self.dispatch_array_splice_set(
            site,
            state,
            ArraySpliceStage::MoveSet,
            snapshot.receiver,
            key.into(),
            value,
        )
    }

    /// Deletes a move destination when the corresponding source is absent.
    fn dispatch_array_splice_move_delete(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        let (_, to) = splice_move_indices(snapshot);
        self.dispatch_array_splice_delete(
            site,
            state,
            ArraySpliceStage::MoveDelete,
            snapshot.receiver,
            safe_integer_value(to),
        )
    }

    /// Advances a completed move according to the previously selected direction.
    fn finish_array_splice_move(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.update_array_splice_scalars(state, |pending| {
            if pending.items.len() as u64 <= pending.delete_count {
                pending.cursor += 1;
            } else {
                pending.cursor -= 1;
            }
        })?;
        self.advance_array_splice_move(site, state)
    }

    /// Deletes the now-unused suffix after a shrinking forward move.
    fn advance_array_splice_tail_delete(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_splice_snapshot(state)?;
        if snapshot.cursor <= snapshot.new_len {
            return self.begin_array_splice_insert(site, state);
        }
        let index = snapshot.cursor - 1;
        self.dispatch_array_splice_delete(
            site,
            state,
            ArraySpliceStage::TailDelete,
            snapshot.receiver,
            safe_integer_value(index),
        )
    }

    /// Decrements the suffix cursor only after DeletePropertyOrThrow succeeds.
    fn finish_array_splice_tail(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.update_array_splice_scalars(state, |pending| pending.cursor -= 1)?;
        self.advance_array_splice_tail_delete(site, state)
    }

    /// Resets the cursor and begins left-to-right insertion.
    fn begin_array_splice_insert(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.update_array_splice_scalars(state, |pending| pending.cursor = 0)?;
        self.advance_array_splice_insert(site, state)
    }

    /// Sets each captured item, then always performs the final observable length Set.
    fn advance_array_splice_insert(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.root_array_splice_state(site, state)?;
        let snapshot = self.array_splice_snapshot(state)?;
        if snapshot.cursor >= snapshot.item_count {
            let length = self.length_atom()?;
            return self.dispatch_array_splice_set(
                site,
                state,
                ArraySpliceStage::FinalLength,
                snapshot.receiver,
                length.into(),
                safe_integer_value(snapshot.new_len),
            );
        }
        let item = self.array_splice_item(state, snapshot.cursor as usize)?;
        let key = self.safe_integer_property_atom(snapshot.start + snapshot.cursor)?;
        self.dispatch_array_splice_set(
            site,
            state,
            ArraySpliceStage::InsertSet,
            snapshot.receiver,
            key.into(),
            item,
        )
    }

    /// Advances the item cursor only after Set succeeds.
    fn finish_array_splice_insert(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        self.update_array_splice_scalars(state, |pending| pending.cursor += 1)?;
        self.advance_array_splice_insert(site, state)
    }

    /// Returns the species result after the final length write.
    fn finish_array_splice(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySplice>,
    ) -> Result<(), ExecutionError> {
        let result = self.array_splice_snapshot(state)?.result;
        self.write(site.caller_base, site.destination, result)
    }
}
