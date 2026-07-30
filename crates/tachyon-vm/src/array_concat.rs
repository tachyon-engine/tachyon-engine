//! Resumable `Array.prototype.concat` species and element-copy algorithm.

use core::mem::size_of;

use super::*;

mod support;

/// GC-owned concat inputs and cursor state across observable JavaScript work.
#[derive(Debug)]
pub(crate) struct PendingArrayConcat {
    receiver: Value,
    result: Value,
    current: Value,
    retained: Value,
    constructor: Value,
    arguments: Box<[Value]>,
    source_index: u32,
    source_length: u64,
    element_index: u64,
    next_index: u64,
}

impl Trace for PendingArrayConcat {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.result.trace(tracer);
        self.current.trace(tracer);
        self.retained.trace(tracer);
        self.constructor.trace(tracer);
        self.arguments.trace(tracer);
    }
}

impl GcExternalMemory for PendingArrayConcat {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.arguments.len().saturating_mul(size_of::<Value>())
    }
}

#[derive(Clone, Copy)]
struct ArrayConcatSnapshot {
    receiver: Value,
    result: Value,
    current: Value,
    source_index: u32,
    source_count: u32,
    source_length: u64,
    element_index: u64,
    next_index: u64,
}

impl Isolate {
    /// Captures concat arguments before beginning ArraySpeciesCreate.
    pub(crate) fn begin_array_concat(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let count = usize::try_from(site.argument_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            );
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_array_concat_state(PendingArrayConcat {
            receiver,
            result: undefined,
            current: receiver,
            retained: undefined,
            constructor: undefined,
            arguments: arguments.into_boxed_slice(),
            source_index: 0,
            source_length: 0,
            element_index: 0,
            next_index: 0,
        })?;
        let site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_concat_state(site, state)?;
        self.begin_array_concat_species(site, state)
    }

    /// Routes each observable concat completion to its algorithm stage.
    pub(crate) fn resume_array_concat(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_concat_state(site, state)?;
        match stage {
            ArrayConcatStage::SpeciesConstructor => {
                self.resume_array_concat_constructor(site, state, value)
            }
            ArrayConcatStage::SpeciesValue => {
                self.finish_array_concat_species(site, state, value, true)
            }
            ArrayConcatStage::SpeciesConstruct => {
                self.finish_array_concat_construct(site, state, value)
            }
            ArrayConcatStage::Spreadable => self.resume_array_concat_spreadable(site, state, value),
            ArrayConcatStage::Length => self.resume_array_concat_length(site, state, value),
            ArrayConcatStage::ElementHas => {
                self.resume_array_concat_element_has(site, state, value)
            }
            ArrayConcatStage::ElementGet => {
                self.resume_array_concat_element_get(site, state, value)
            }
            ArrayConcatStage::ElementDefine => self.finish_array_concat_element(site, state),
            ArrayConcatStage::ValueDefine => self.finish_array_concat_source(site, state),
            ArrayConcatStage::FinalLength => self.finish_array_concat(site, state),
        }
    }

    /// Continues ToLength after an object-to-primitive conversion.
    pub(crate) fn resume_array_concat_length_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_concat_state(site, state)?;
        self.finish_array_concat_length(site, state, value)
    }

    /// Implements the initial IsArray and constructor lookup of ArraySpeciesCreate.
    fn begin_array_concat_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.array_concat_snapshot(state)?.receiver;
        if !self.is_array_value(receiver)? {
            return self.finish_array_concat_species(
                site,
                state,
                Value::from_immediate(Immediate::Undefined),
                false,
            );
        }
        let constructor = self.constructor_atom()?;
        if let Some((state, value)) = self.dispatch_array_concat_get(
            site,
            state,
            ArrayConcatStage::SpeciesConstructor,
            receiver,
            constructor.into(),
        )? {
            self.resume_array_concat_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Applies cross-Realm intrinsic Array fallback before reading Symbol.species.
    fn resume_array_concat_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return self.finish_array_concat_species(site, state, value, false);
        }
        if self.is_constructor_value(value)? {
            let constructor_realm = self.realm_for_callable(value)?;
            if constructor_realm != self.active_realm
                && self.realm_array_constructor(constructor_realm) == Some(value)
            {
                return self.finish_array_concat_species(
                    site,
                    state,
                    Value::from_immediate(Immediate::Undefined),
                    false,
                );
            }
        }
        self.set_array_concat_value(state, |pending| &mut pending.constructor, value)?;
        let species = self
            .agent
            .well_known_symbols
            .species
            .expect("Symbol.species initializes before Array");
        let key = self.property_key(species)?;
        if let Some((state, observed)) =
            self.dispatch_array_concat_get(site, state, ArrayConcatStage::SpeciesValue, value, key)?
        {
            self.finish_array_concat_species(site, state, observed, true)?;
        }
        Ok(())
    }

    /// Creates the intrinsic empty Array or invokes a custom species constructor.
    fn finish_array_concat_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        constructor: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined)
            || (from_species && constructor.as_immediate() == Some(Immediate::Null))
        {
            self.root_array_concat_state(site, state)?;
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before concat");
            let result = self.create_array_object_with_prototype(prototype)?;
            let state = self
                .pending_array_concat_reference(self.read(site.caller_base, site.destination)?)?;
            self.set_array_concat_value(state, |pending| &mut pending.result, result)?;
            self.set_array_length_value(result, Value::from_i32(0))?;
            return self.advance_array_concat_source(site, state);
        }
        if constructor.as_immediate() == Some(Immediate::Null) || !self.is_object_value(constructor)
        {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_array_concat_species(site, state, constructor)
    }

    /// Roots the zero length argument while a custom species constructor runs.
    fn construct_array_concat_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_concat_value(state, |pending| &mut pending.constructor, constructor)?;
        self.push_array_concat_parent(
            site,
            state,
            ArrayConcatStage::SpeciesConstruct,
            constructor,
        )?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let prefix = match self.create_apply_argument_prefix(
            constructor,
            undefined,
            vec![Value::from_i32(0)],
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_concat_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_array_concat_parent(
            site,
            state,
            ArrayConcatStage::SpeciesConstruct,
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
        let state = self.pending_array_concat_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_concat_construct(site, state, result)
    }

    /// Validates and publishes a custom species result.
    fn finish_array_concat_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        self.set_array_concat_value(state, |pending| &mut pending.result, result)?;
        self.advance_array_concat_source(site, state)
    }

    /// Selects the receiver or next captured argument and evaluates spreadability.
    fn advance_array_concat_source(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_concat_snapshot(state)?;
        if snapshot.source_index >= snapshot.source_count {
            let length = self.length_atom()?;
            return self.dispatch_array_concat_set(
                site,
                state,
                ArrayConcatStage::FinalLength,
                snapshot.result,
                length.into(),
                safe_integer_value(snapshot.next_index),
            );
        }
        let current = if snapshot.source_index == 0 {
            snapshot.receiver
        } else {
            self.array_concat_argument(state, snapshot.source_index - 1)?
        };
        self.set_array_concat_value(state, |pending| &mut pending.current, current)?;
        if !self.is_object_value(current) {
            return self.begin_array_concat_value_define(site, state);
        }
        let spreadable = self
            .agent
            .well_known_symbols
            .is_concat_spreadable
            .expect("Symbol.isConcatSpreadable initializes before Array");
        let key = self.property_key(spreadable)?;
        if let Some((state, value)) =
            self.dispatch_array_concat_get(site, state, ArrayConcatStage::Spreadable, current, key)?
        {
            self.resume_array_concat_spreadable(site, state, value)?;
        }
        Ok(())
    }

    /// Applies IsConcatSpreadable and branches into scalar or array-like copying.
    fn resume_array_concat_spreadable(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let current = self.array_concat_snapshot(state)?.current;
        let spreadable = if value.as_immediate() == Some(Immediate::Undefined) {
            self.is_array_value(current)?
        } else {
            self.is_truthy_value(value)?
        };
        if !spreadable {
            return self.begin_array_concat_value_define(site, state);
        }
        let length = self.length_atom()?;
        if let Some((state, value)) = self.dispatch_array_concat_get(
            site,
            state,
            ArrayConcatStage::Length,
            current,
            length.into(),
        )? {
            self.resume_array_concat_length(site, state, value)?;
        }
        Ok(())
    }

    /// Dispatches observable object conversion or completes primitive ToLength.
    fn resume_array_concat_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayConcatLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_concat_length(site, state, value)
    }

    /// Stores ToLength after enforcing the safe-integer output bound.
    fn finish_array_concat_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let length = concat_to_length(self.convert_to_number(value)?)?;
        let next = self.array_concat_snapshot(state)?.next_index;
        next.checked_add(length)
            .filter(|value| *value <= MAX_SAFE_INTEGER)
            .ok_or(ExecutionError::ArrayLengthOverflow)?;
        self.update_array_concat_scalars(state, |pending| {
            pending.source_length = length;
            pending.element_index = 0;
        })?;
        self.advance_array_concat_element(site, state)
    }

    /// Iterates a spread source with HasProperty/Get/CreateDataProperty ordering.
    fn advance_array_concat_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.array_concat_snapshot(state)?;
            if snapshot.element_index >= snapshot.source_length {
                self.update_array_concat_scalars(state, |pending| {
                    pending.next_index += pending.source_length;
                    pending.source_index += 1;
                    pending.source_length = 0;
                    pending.element_index = 0;
                })?;
                return self.advance_array_concat_source(site, state);
            }
            let key = safe_integer_value(snapshot.element_index);
            let Some((state, has)) = self.dispatch_array_concat_has(
                site,
                state,
                ArrayConcatStage::ElementHas,
                snapshot.current,
                key,
            )?
            else {
                return Ok(());
            };
            if self.is_truthy_value(has)? {
                let Some(state) = self.copy_synchronous_array_concat_element(site, state)? else {
                    return Ok(());
                };
                self.update_array_concat_scalars(state, |pending| pending.element_index += 1)?;
                continue;
            }
            self.skip_array_concat_holes(state)?;
        }
    }

    /// Copies one present element without recursively re-entering the concat driver.
    fn copy_synchronous_array_concat_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<Option<GcRef<PendingArrayConcat>>, ExecutionError> {
        let snapshot = self.array_concat_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.element_index)?;
        let Some((state, value)) = self.dispatch_array_concat_get(
            site,
            state,
            ArrayConcatStage::ElementGet,
            snapshot.current,
            key.into(),
        )?
        else {
            return Ok(None);
        };
        self.prepare_array_concat_define(
            site,
            state,
            ArrayConcatStage::ElementDefine,
            snapshot.next_index + snapshot.element_index,
            value,
        )
    }

    /// Branches the completed HasProperty operation without materializing holes.
    fn resume_array_concat_element_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_truthy_value(value)? {
            return self.dispatch_array_concat_element_get(site, state);
        }
        self.skip_array_concat_holes(state)?;
        self.advance_array_concat_element(site, state)
    }

    /// Gets one known-present source property.
    fn dispatch_array_concat_element_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_concat_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.element_index)?;
        if let Some((state, value)) = self.dispatch_array_concat_get(
            site,
            state,
            ArrayConcatStage::ElementGet,
            snapshot.current,
            key.into(),
        )? {
            self.resume_array_concat_element_get(site, state, value)?;
        }
        Ok(())
    }

    /// Defines one present spread element on the species result.
    fn resume_array_concat_element_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_concat_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_concat_snapshot(state)?;
        let index = snapshot.next_index + snapshot.element_index;
        self.begin_array_concat_define(site, state, ArrayConcatStage::ElementDefine, index, value)
    }

    /// Advances the spread cursor only after CreateDataPropertyOrThrow succeeds.
    fn finish_array_concat_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        self.update_array_concat_scalars(state, |pending| pending.element_index += 1)?;
        self.advance_array_concat_element(site, state)
    }

    /// Defines a non-spread source as one result element.
    fn begin_array_concat_value_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_concat_snapshot(state)?;
        if snapshot.next_index >= MAX_SAFE_INTEGER {
            return Err(ExecutionError::ArrayLengthOverflow);
        }
        self.begin_array_concat_define(
            site,
            state,
            ArrayConcatStage::ValueDefine,
            snapshot.next_index,
            snapshot.current,
        )
    }

    /// Performs CreateDataPropertyOrThrow for either concat copy branch.
    fn begin_array_concat_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        index: u64,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let Some(state) = self.prepare_array_concat_define(site, state, stage, index, value)?
        else {
            return Ok(());
        };
        match stage {
            ArrayConcatStage::ElementDefine => self.finish_array_concat_element(site, state),
            ArrayConcatStage::ValueDefine => self.finish_array_concat_source(site, state),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Performs one define operation and reports synchronous completion to the caller's loop.
    fn prepare_array_concat_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
        stage: ArrayConcatStage,
        index: u64,
        value: Value,
    ) -> Result<Option<GcRef<PendingArrayConcat>>, ExecutionError> {
        self.root_array_concat_state(site, state)?;
        self.set_array_concat_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_concat_snapshot(state)?;
        let key = self.safe_integer_property_atom(index)?;
        let descriptor = DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        if self.is_proxy_value(snapshot.result) {
            return self.dispatch_array_concat_define(
                site,
                state,
                stage,
                snapshot.result,
                key.into(),
                descriptor.into(),
            );
        }
        self.define_data_property(snapshot.result, key, descriptor)?;
        let state =
            self.pending_array_concat_reference(self.read(site.caller_base, site.destination)?)?;
        Ok(Some(state))
    }

    /// Advances to the next captured source after defining one scalar value.
    fn finish_array_concat_source(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        self.update_array_concat_scalars(state, |pending| {
            pending.next_index += 1;
            pending.source_index += 1;
        })?;
        self.advance_array_concat_source(site, state)
    }

    /// Publishes the final species result after its length Set succeeds.
    fn finish_array_concat(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayConcat>,
    ) -> Result<(), ExecutionError> {
        let result = self.array_concat_snapshot(state)?.result;
        self.write(site.caller_base, site.destination, result)
    }
}

#[inline(always)]
fn concat_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        Ok(0)
    } else if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
        Ok(MAX_SAFE_INTEGER)
    } else {
        Ok(number.floor() as u64)
    }
}
