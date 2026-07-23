//! Resumable `Array.prototype.flatMap` mapping and depth-one flattening.

use super::*;

mod support;

/// GC-owned flatMap inputs and outer/inner cursors across observable JavaScript work.
#[derive(Debug)]
pub(crate) struct PendingArrayFlatMap {
    receiver: Value,
    callback: Value,
    this_argument: Value,
    result: Value,
    retained: Value,
    constructor: Value,
    current: Value,
    source_length: u64,
    source_index: u64,
    inner_length: u64,
    inner_index: u64,
    target_index: u64,
    inner_active: bool,
}

impl Trace for PendingArrayFlatMap {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.callback.trace(tracer);
        self.this_argument.trace(tracer);
        self.result.trace(tracer);
        self.retained.trace(tracer);
        self.constructor.trace(tracer);
        self.current.trace(tracer);
    }
}

#[derive(Clone, Copy)]
struct ArrayFlatMapSnapshot {
    receiver: Value,
    callback: Value,
    this_argument: Value,
    result: Value,
    current: Value,
    source_length: u64,
    source_index: u64,
    inner_length: u64,
    inner_index: u64,
    target_index: u64,
    inner_active: bool,
}

impl Isolate {
    /// Captures flatMap inputs before the observable length lookup.
    pub(crate) fn begin_array_flat_map(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let callback = self.call_argument(site, 0)?.unwrap_or(undefined);
        let this_argument = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_array_flat_map_state(PendingArrayFlatMap {
            receiver,
            callback,
            this_argument,
            result: undefined,
            retained: undefined,
            constructor: undefined,
            current: undefined,
            source_length: 0,
            source_index: 0,
            inner_length: 0,
            inner_index: 0,
            target_index: 0,
            inner_active: false,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_flat_map_state(native_site, state)?;
        let length = self.length_atom()?;
        if let Some((state, value)) = self.dispatch_array_flat_map_get(
            native_site,
            state,
            ArrayFlatMapStage::Length,
            receiver,
            length.into(),
        )? {
            self.resume_array_flat_map(native_site, state, ArrayFlatMapStage::Length, value)?;
        }
        Ok(())
    }

    /// Routes each observable flatMap completion to its explicit algorithm stage.
    pub(crate) fn resume_array_flat_map(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        stage: ArrayFlatMapStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_flat_map_state(site, state)?;
        match stage {
            ArrayFlatMapStage::Length => self.resume_array_flat_map_length(site, state, value),
            ArrayFlatMapStage::SpeciesConstructor => {
                self.resume_array_flat_map_constructor(site, state, value)
            }
            ArrayFlatMapStage::SpeciesValue => {
                self.finish_array_flat_map_species(site, state, value, true)
            }
            ArrayFlatMapStage::SpeciesConstruct => {
                self.finish_array_flat_map_construct(site, state, value)
            }
            ArrayFlatMapStage::SourceHas => {
                self.resume_array_flat_map_source_has(site, state, value)
            }
            ArrayFlatMapStage::SourceGet => self.call_array_flat_map_callback(site, state, value),
            ArrayFlatMapStage::Callback => self.finish_array_flat_map_mapped(site, state, value),
            ArrayFlatMapStage::InnerLength => {
                self.resume_array_flat_map_inner_length(site, state, value)
            }
            ArrayFlatMapStage::InnerHas => self.resume_array_flat_map_inner_has(site, state, value),
            ArrayFlatMapStage::InnerGet => self.write_array_flat_map_value(site, state, value),
            ArrayFlatMapStage::Define => self.finish_array_flat_map_define(site, state),
        }
    }

    /// Continues either ToLength conversion after object-to-primitive work.
    pub(crate) fn resume_array_flat_map_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_flat_map_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayFlatMapLength => {
                self.finish_array_flat_map_length(site, state, value)
            }
            ConversionConsumer::ArrayFlatMapInnerLength => {
                self.finish_array_flat_map_inner_length(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Dispatches ToLength for the observed outer length value.
    fn resume_array_flat_map_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayFlatMapLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_flat_map_length(site, state, value)
    }

    /// Stores the source length, then validates mapper callability before species lookup.
    fn finish_array_flat_map_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(value)?;
        let length = self.array_flat_map_to_length(number)?;
        self.update_array_flat_map_scalars(state, |pending| pending.source_length = length)?;
        let callback = self.array_flat_map_snapshot(state)?.callback;
        self.resolve_function_object(callback)?;
        self.begin_array_flat_map_species(site, state)
    }

    /// Implements IsArray and observable constructor lookup for ArraySpeciesCreate(O, 0).
    fn begin_array_flat_map_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.array_flat_map_snapshot(state)?.receiver;
        if !self.is_array_value(receiver)? {
            return self.finish_array_flat_map_species(
                site,
                state,
                Value::from_immediate(Immediate::Undefined),
                false,
            );
        }
        let constructor = self.constructor_atom()?;
        if let Some((state, value)) = self.dispatch_array_flat_map_get(
            site,
            state,
            ArrayFlatMapStage::SpeciesConstructor,
            receiver,
            constructor.into(),
        )? {
            self.resume_array_flat_map_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Applies the cross-Realm intrinsic Array fallback before reading Symbol.species.
    fn resume_array_flat_map_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return self.finish_array_flat_map_species(site, state, value, false);
        }
        if self.is_constructor_value(value)? {
            let constructor_realm = self.realm_for_callable(value)?;
            if constructor_realm != self.active_realm
                && self.realm_array_constructor(constructor_realm) == Some(value)
            {
                return self.finish_array_flat_map_species(
                    site,
                    state,
                    Value::from_immediate(Immediate::Undefined),
                    false,
                );
            }
        }
        self.set_array_flat_map_value(state, |pending| &mut pending.constructor, value)?;
        let species = self
            .realm
            .well_known_symbols
            .species
            .expect("Symbol.species initializes before Array");
        let key = self.property_key(species)?;
        if let Some((state, observed)) = self.dispatch_array_flat_map_get(
            site,
            state,
            ArrayFlatMapStage::SpeciesValue,
            value,
            key,
        )? {
            self.finish_array_flat_map_species(site, state, observed, true)?;
        }
        Ok(())
    }

    /// Creates the intrinsic empty Array or invokes a custom species constructor.
    fn finish_array_flat_map_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        constructor: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined)
            || (from_species && constructor.as_immediate() == Some(Immediate::Null))
        {
            self.root_array_flat_map_state(site, state)?;
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before flatMap");
            let result = self.create_array_object_with_prototype(prototype)?;
            let state = self
                .pending_array_flat_map_reference(self.read(site.caller_base, site.destination)?)?;
            self.set_array_flat_map_value(state, |pending| &mut pending.result, result)?;
            self.set_array_length_value(result, Value::from_i32(0))?;
            return self.advance_array_flat_map_source(site, state);
        }
        if constructor.as_immediate() == Some(Immediate::Null) || !self.is_object_value(constructor)
        {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_array_flat_map_species(site, state, constructor)
    }

    /// Calls a custom species constructor with the single zero length argument.
    fn construct_array_flat_map_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_flat_map_value(state, |pending| &mut pending.constructor, constructor)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        arguments.push(Value::from_i32(0));
        self.push_array_flat_map_parent(
            site,
            state,
            ArrayFlatMapStage::SpeciesConstruct,
            constructor,
        )?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, arguments) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_flat_map_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_array_flat_map_parent(
            site,
            state,
            ArrayFlatMapStage::SpeciesConstruct,
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
        let state = self.pending_array_flat_map_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_flat_map_construct(site, state, result)
    }

    /// Validates a custom species result before beginning source iteration.
    fn finish_array_flat_map_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        self.set_array_flat_map_value(state, |pending| &mut pending.result, result)?;
        self.advance_array_flat_map_source(site, state)
    }

    /// Iterates present outer properties in ascending order without Rust recursion.
    fn advance_array_flat_map_source(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.root_array_flat_map_state(site, state)?;
            let snapshot = self.array_flat_map_snapshot(state)?;
            if snapshot.source_index >= snapshot.source_length {
                return self.write(site.caller_base, site.destination, snapshot.result);
            }
            let index = snapshot.source_index;
            self.update_array_flat_map_scalars(state, |pending| pending.source_index += 1)?;
            let Some((state, has)) = self.dispatch_array_flat_map_has(
                site,
                state,
                ArrayFlatMapStage::SourceHas,
                snapshot.receiver,
                safe_integer_value(index),
            )?
            else {
                return Ok(());
            };
            if self.is_truthy_value(has)? {
                return self.dispatch_array_flat_map_source_get(site, state);
            }
            self.skip_array_flat_map_holes(state, false)?;
        }
    }

    /// Handles one outer HasProperty completion and skips only proven ordinary holes.
    fn resume_array_flat_map_source_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_truthy_value(value)? {
            return self.dispatch_array_flat_map_source_get(site, state);
        }
        self.skip_array_flat_map_holes(state, false)?;
        self.advance_array_flat_map_source(site, state)
    }

    /// Dispatches Get for the current present outer property.
    fn dispatch_array_flat_map_source_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_flat_map_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.source_index - 1)?;
        if let Some((state, value)) = self.dispatch_array_flat_map_get(
            site,
            state,
            ArrayFlatMapStage::SourceGet,
            snapshot.receiver,
            key.into(),
        )? {
            self.call_array_flat_map_callback(site, state, value)?;
        }
        Ok(())
    }

    /// Calls mapper with `(element, sourceIndex, receiver)` and the captured thisArg.
    fn call_array_flat_map_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        element: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_flat_map_value(state, |pending| &mut pending.retained, element)?;
        let snapshot = self.array_flat_map_snapshot(state)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(3)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        arguments.push(element);
        arguments.push(safe_integer_value(snapshot.source_index - 1));
        arguments.push(snapshot.receiver);
        self.push_array_flat_map_parent(site, state, ArrayFlatMapStage::Callback, element)?;
        let prefix = match self.create_apply_argument_prefix(
            snapshot.callback,
            snapshot.this_argument,
            arguments,
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_flat_map_reference(rooted.first())?;
        self.push_array_flat_map_parent(
            site,
            state,
            ArrayFlatMapStage::Callback,
            Value::from_heap_ref(prefix.raw()),
        )?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: snapshot.callback,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 3,
            argument_count: 3,
            this_value: snapshot.this_argument,
            new_target: Value::from_immediate(Immediate::Undefined),
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
                .expect("flatMap mapper publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_flat_map_reference(rooted.first())?;
        let mapped = self.read(site.caller_base, site.destination)?;
        self.finish_array_flat_map_mapped(site, state, mapped)
    }

    /// Selects direct output or begins depth-one flattening of a mapped Array.
    fn finish_array_flat_map_mapped(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        mapped: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_flat_map_value(state, |pending| &mut pending.current, mapped)?;
        self.root_array_flat_map_state(site, state)?;
        if !self.is_array_value(mapped)? {
            self.update_array_flat_map_scalars(state, |pending| pending.inner_active = false)?;
            return self.write_array_flat_map_value(site, state, mapped);
        }
        let length = self.length_atom()?;
        if let Some((state, value)) = self.dispatch_array_flat_map_get(
            site,
            state,
            ArrayFlatMapStage::InnerLength,
            mapped,
            length.into(),
        )? {
            self.resume_array_flat_map_inner_length(site, state, value)?;
        }
        Ok(())
    }

    /// Dispatches ToLength for a mapped Array's observed length.
    fn resume_array_flat_map_inner_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayFlatMapInnerLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_flat_map_inner_length(site, state, value)
    }

    /// Stores the mapped Array length and starts its depth-zero scan.
    fn finish_array_flat_map_inner_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(value)?;
        let length = self.array_flat_map_to_length(number)?;
        self.update_array_flat_map_scalars(state, |pending| {
            pending.inner_length = length;
            pending.inner_index = 0;
            pending.inner_active = true;
        })?;
        self.advance_array_flat_map_inner(site, state)
    }

    /// Iterates a mapped Array's present properties at depth zero.
    fn advance_array_flat_map_inner(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.root_array_flat_map_state(site, state)?;
            let snapshot = self.array_flat_map_snapshot(state)?;
            if snapshot.inner_index >= snapshot.inner_length {
                self.update_array_flat_map_scalars(state, |pending| pending.inner_active = false)?;
                return self.advance_array_flat_map_source(site, state);
            }
            let index = snapshot.inner_index;
            self.update_array_flat_map_scalars(state, |pending| pending.inner_index += 1)?;
            let Some((state, has)) = self.dispatch_array_flat_map_has(
                site,
                state,
                ArrayFlatMapStage::InnerHas,
                snapshot.current,
                safe_integer_value(index),
            )?
            else {
                return Ok(());
            };
            if self.is_truthy_value(has)? {
                return self.dispatch_array_flat_map_inner_get(site, state);
            }
            self.skip_array_flat_map_holes(state, true)?;
        }
    }

    /// Handles one mapped Array HasProperty completion.
    fn resume_array_flat_map_inner_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_truthy_value(value)? {
            return self.dispatch_array_flat_map_inner_get(site, state);
        }
        self.skip_array_flat_map_holes(state, true)?;
        self.advance_array_flat_map_inner(site, state)
    }

    /// Dispatches Get for the current present property of the mapped Array.
    fn dispatch_array_flat_map_inner_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_flat_map_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.inner_index - 1)?;
        if let Some((state, value)) = self.dispatch_array_flat_map_get(
            site,
            state,
            ArrayFlatMapStage::InnerGet,
            snapshot.current,
            key.into(),
        )? {
            self.write_array_flat_map_value(site, state, value)?;
        }
        Ok(())
    }

    /// Creates one dense target data property, preserving value across key allocation.
    fn write_array_flat_map_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_flat_map_value(state, |pending| &mut pending.retained, value)?;
        self.root_array_flat_map_state(site, state)?;
        let snapshot = self.array_flat_map_snapshot(state)?;
        if snapshot.target_index >= MAX_SAFE_INTEGER {
            return Err(ExecutionError::ArrayLengthOverflow);
        }
        let key = self.safe_integer_property_atom(snapshot.target_index)?;
        let descriptor = DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        if self.is_proxy_value(snapshot.result) {
            return self.dispatch_array_flat_map_define(
                site,
                state,
                snapshot.result,
                key.into(),
                descriptor.into(),
            );
        }
        self.define_data_property(snapshot.result, key, descriptor)?;
        let state =
            self.pending_array_flat_map_reference(self.read(site.caller_base, site.destination)?)?;
        self.finish_array_flat_map_define(site, state)
    }

    /// Commits the target cursor only after CreateDataPropertyOrThrow succeeds.
    fn finish_array_flat_map_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlatMap>,
    ) -> Result<(), ExecutionError> {
        let inner_active = self.array_flat_map_snapshot(state)?.inner_active;
        self.update_array_flat_map_scalars(state, |pending| pending.target_index += 1)?;
        if inner_active {
            self.advance_array_flat_map_inner(site, state)
        } else {
            self.advance_array_flat_map_source(site, state)
        }
    }
}
