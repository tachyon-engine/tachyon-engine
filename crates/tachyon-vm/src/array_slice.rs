//! Resumable `Array.prototype.slice` species and element-copy algorithm.

use super::*;

mod support;
use support::{relative_slice_index, slice_integer, slice_to_length};

/// GC-owned slice inputs and cursor state across observable JavaScript work.
#[derive(Debug)]
pub(crate) struct PendingArraySlice {
    receiver: Value,
    result: Value,
    retained: Value,
    constructor: Value,
    start_argument: Value,
    end_argument: Value,
    length: u64,
    start: u64,
    final_index: u64,
    source_index: u64,
    target_index: u64,
}

impl Trace for PendingArraySlice {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.result.trace(tracer);
        self.retained.trace(tracer);
        self.constructor.trace(tracer);
        self.start_argument.trace(tracer);
        self.end_argument.trace(tracer);
    }
}

#[derive(Clone, Copy)]
struct ArraySliceSnapshot {
    receiver: Value,
    result: Value,
    length: u64,
    start: u64,
    final_index: u64,
    source_index: u64,
    target_index: u64,
}

impl Isolate {
    /// Captures slice arguments before the first observable length lookup.
    pub(crate) fn begin_array_slice(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let start_argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let end_argument = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_array_slice_state(PendingArraySlice {
            receiver,
            result: undefined,
            retained: undefined,
            constructor: undefined,
            start_argument,
            end_argument,
            length: 0,
            start: 0,
            final_index: 0,
            source_index: 0,
            target_index: 0,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_slice_state(native_site, state)?;
        let length = self.length_atom()?;
        if let Some((state, value)) = self.dispatch_array_slice_get(
            native_site,
            state,
            ArraySliceStage::Length,
            receiver,
            length.into(),
        )? {
            self.resume_array_slice(native_site, state, ArraySliceStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes every observable slice completion to its explicit algorithm stage.
    pub(crate) fn resume_array_slice(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        stage: ArraySliceStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_slice_state(site, state)?;
        match stage {
            ArraySliceStage::Length => self.resume_array_slice_length(site, state, value),
            ArraySliceStage::SpeciesConstructor => {
                self.resume_array_slice_constructor(site, state, value)
            }
            ArraySliceStage::SpeciesValue => {
                self.finish_array_slice_species(site, state, value, true)
            }
            ArraySliceStage::SpeciesConstruct => {
                self.finish_array_slice_construct(site, state, value)
            }
            ArraySliceStage::ElementHas => self.resume_array_slice_has(site, state, value),
            ArraySliceStage::ElementGet => self.resume_array_slice_get(site, state, value),
            ArraySliceStage::ElementDefine => self.finish_array_slice_element(site, state),
            ArraySliceStage::FinalLength => self.finish_array_slice(site, state),
        }
    }

    /// Continues length, start, or end conversion after object-to-primitive work.
    pub(crate) fn resume_array_slice_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_slice_state(site, state)?;
        match consumer {
            ConversionConsumer::ArraySliceLength => {
                self.finish_array_slice_length(site, state, value)
            }
            ConversionConsumer::ArraySliceStart => {
                self.finish_array_slice_start(site, state, value)
            }
            ConversionConsumer::ArraySliceEnd => self.finish_array_slice_end(site, state, value),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Dispatches ToLength for the observed length value.
    fn resume_array_slice_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArraySliceLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_slice_length(site, state, value)
    }

    /// Stores ToLength and begins ToIntegerOrInfinity(start).
    fn finish_array_slice_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = slice_to_length(self.convert_to_number(value)?)?;
        self.update_array_slice_scalars(state, |pending| pending.length = length)?;
        let start = self.array_slice_argument(state, true)?;
        if self.is_object_value(start) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArraySliceStart,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                start,
                site.call_site,
            );
        }
        self.finish_array_slice_start(site, state, start)
    }

    /// Clamps start and begins the optional end conversion.
    fn finish_array_slice_start(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let snapshot = self.array_slice_snapshot(state)?;
        let start = relative_slice_index(snapshot.length, slice_integer(number));
        self.update_array_slice_scalars(state, |pending| pending.start = start)?;
        let end = self.array_slice_argument(state, false)?;
        if end.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_array_slice_indices(site, state, snapshot.length);
        }
        if self.is_object_value(end) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArraySliceEnd,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                end,
                site.call_site,
            );
        }
        self.finish_array_slice_end(site, state, end)
    }

    /// Converts and clamps an explicitly supplied end value.
    fn finish_array_slice_end(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let length = self.array_slice_snapshot(state)?.length;
        self.finish_array_slice_indices(
            site,
            state,
            relative_slice_index(length, slice_integer(number)),
        )
    }

    /// Freezes the copy interval and begins ArraySpeciesCreate.
    fn finish_array_slice_indices(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        final_index: u64,
    ) -> Result<(), ExecutionError> {
        let start = self.array_slice_snapshot(state)?.start;
        self.update_array_slice_scalars(state, |pending| {
            pending.final_index = final_index;
            pending.source_index = start;
            pending.target_index = 0;
        })?;
        self.begin_array_slice_species(site, state)
    }

    /// Implements IsArray and the observable constructor lookup of ArraySpeciesCreate.
    fn begin_array_slice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.array_slice_snapshot(state)?.receiver;
        if !self.is_array_value(receiver)? {
            return self.finish_array_slice_species(
                site,
                state,
                Value::from_immediate(Immediate::Undefined),
                false,
            );
        }
        let constructor = self.constructor_atom()?;
        if let Some((state, value)) = self.dispatch_array_slice_get(
            site,
            state,
            ArraySliceStage::SpeciesConstructor,
            receiver,
            constructor.into(),
        )? {
            self.resume_array_slice_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Applies cross-Realm intrinsic Array fallback before reading Symbol.species.
    fn resume_array_slice_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return self.finish_array_slice_species(site, state, value, false);
        }
        if self.is_constructor_value(value)? {
            let constructor_realm = self.realm_for_callable(value)?;
            if constructor_realm != self.active_realm
                && self.realm_array_constructor(constructor_realm) == Some(value)
            {
                return self.finish_array_slice_species(
                    site,
                    state,
                    Value::from_immediate(Immediate::Undefined),
                    false,
                );
            }
        }
        self.set_array_slice_value(state, |pending| &mut pending.constructor, value)?;
        let species = self
            .realm
            .well_known_symbols
            .species
            .expect("Symbol.species initializes before Array");
        let key = self.property_key(species)?;
        if let Some((state, observed)) =
            self.dispatch_array_slice_get(site, state, ArraySliceStage::SpeciesValue, value, key)?
        {
            self.finish_array_slice_species(site, state, observed, true)?;
        }
        Ok(())
    }

    /// Creates an intrinsic Array or invokes the selected custom species constructor.
    fn finish_array_slice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        constructor: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined)
            || (from_species && constructor.as_immediate() == Some(Immediate::Null))
        {
            self.root_array_slice_state(site, state)?;
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before slice");
            let result = self.create_array_object_with_prototype(prototype)?;
            let state =
                self.pending_array_slice_reference(self.read(site.caller_base, site.destination)?)?;
            self.set_array_slice_value(state, |pending| &mut pending.result, result)?;
            let snapshot = self.array_slice_snapshot(state)?;
            let count = snapshot.final_index.saturating_sub(snapshot.start);
            self.set_array_length_value(result, safe_integer_value(count))?;
            return self.advance_array_slice(site, state);
        }
        if constructor.as_immediate() == Some(Immediate::Null) || !self.is_object_value(constructor)
        {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_array_slice_species(site, state, constructor)
    }

    /// Roots the exact one-element argument prefix while a custom constructor runs.
    fn construct_array_slice_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_slice_value(state, |pending| &mut pending.constructor, constructor)?;
        let snapshot = self.array_slice_snapshot(state)?;
        let count = snapshot.final_index.saturating_sub(snapshot.start);
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        arguments.push(safe_integer_value(count));
        let undefined = Value::from_immediate(Immediate::Undefined);
        self.push_array_slice_parent(site, state, ArraySliceStage::SpeciesConstruct, constructor)?;
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, arguments) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_slice_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_array_slice_parent(
            site,
            state,
            ArraySliceStage::SpeciesConstruct,
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
        let state = self.pending_array_slice_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_slice_construct(site, state, result)
    }

    /// Validates the custom species result before beginning element copying.
    fn finish_array_slice_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        self.set_array_slice_value(state, |pending| &mut pending.result, result)?;
        self.advance_array_slice(site, state)
    }

    /// Copies each present property through HasProperty/Get/CreateDataPropertyOrThrow.
    fn advance_array_slice(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.root_array_slice_state(site, state)?;
            let snapshot = self.array_slice_snapshot(state)?;
            if snapshot.source_index >= snapshot.final_index {
                let length = self.length_atom()?;
                return self.dispatch_array_slice_set(
                    site,
                    state,
                    snapshot.result,
                    length.into(),
                    safe_integer_value(snapshot.target_index),
                );
            }
            let Some((state, has)) = self.dispatch_array_slice_has(
                site,
                state,
                snapshot.receiver,
                safe_integer_value(snapshot.source_index),
            )?
            else {
                return Ok(());
            };
            if self.is_truthy_value(has)? {
                return self.dispatch_array_slice_element_get(site, state);
            }
            self.advance_array_slice_cursor(state)?;
        }
    }

    /// Handles an asynchronous HasProperty completion without filling holes.
    fn resume_array_slice_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_truthy_value(value)? {
            return self.dispatch_array_slice_element_get(site, state);
        }
        self.advance_array_slice_cursor(state)?;
        self.advance_array_slice(site, state)
    }

    /// Dispatches Get for the present source property at the current cursor.
    fn dispatch_array_slice_element_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_slice_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.source_index)?;
        if let Some((state, value)) = self.dispatch_array_slice_get(
            site,
            state,
            ArraySliceStage::ElementGet,
            snapshot.receiver,
            key.into(),
        )? {
            self.resume_array_slice_get(site, state, value)?;
        }
        Ok(())
    }

    /// Defines one present value on the species result with data attributes true.
    fn resume_array_slice_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_slice_state(site, state)?;
        self.set_array_slice_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_slice_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.target_index)?;
        let descriptor = DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        if self.is_proxy_value(snapshot.result) {
            return self.dispatch_array_slice_define(
                site,
                state,
                snapshot.result,
                key.into(),
                descriptor.into(),
            );
        }
        self.define_data_property(snapshot.result, key, descriptor)?;
        let state =
            self.pending_array_slice_reference(self.read(site.caller_base, site.destination)?)?;
        self.finish_array_slice_element(site, state)
    }

    /// Advances both cursors only after a successful element definition.
    fn finish_array_slice_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
    ) -> Result<(), ExecutionError> {
        self.advance_array_slice_cursor(state)?;
        self.advance_array_slice(site, state)
    }

    /// Publishes the species result after the final observable length Set.
    fn finish_array_slice(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArraySlice>,
    ) -> Result<(), ExecutionError> {
        let result = self.array_slice_snapshot(state)?.result;
        self.write(site.caller_base, site.destination, result)
    }
}
