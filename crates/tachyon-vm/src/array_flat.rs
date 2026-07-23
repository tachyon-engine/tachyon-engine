//! Resumable `Array.prototype.flat` species creation and iterative flattening.

use core::mem::size_of;

use super::*;

mod support;

/// One suspended source traversal in the iterative FlattenIntoArray machine.
#[derive(Clone, Copy, Debug)]
struct ArrayFlatFrame {
    source: Value,
    length: u64,
    index: u64,
    depth: u64,
    infinite_depth: bool,
}

impl Trace for ArrayFlatFrame {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.source.trace(tracer);
    }
}

/// GC-owned flat inputs, explicit frames, and cursors across observable work.
#[derive(Debug)]
pub(crate) struct PendingArrayFlat {
    receiver: Value,
    result: Value,
    retained: Value,
    constructor: Value,
    depth_argument: Value,
    source: Value,
    frames: Box<[ArrayFlatFrame]>,
    length: u64,
    index: u64,
    target_index: u64,
    depth: u64,
    frame_count: usize,
    infinite_depth: bool,
}

impl Trace for PendingArrayFlat {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.result.trace(tracer);
        self.retained.trace(tracer);
        self.constructor.trace(tracer);
        self.depth_argument.trace(tracer);
        self.source.trace(tracer);
        for frame in &mut self.frames[..self.frame_count] {
            frame.trace(tracer);
        }
    }
}

impl GcExternalMemory for PendingArrayFlat {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.frames
            .len()
            .saturating_mul(size_of::<ArrayFlatFrame>())
    }
}

#[derive(Clone, Copy)]
struct ArrayFlatSnapshot {
    receiver: Value,
    result: Value,
    retained: Value,
    constructor: Value,
    depth_argument: Value,
    source: Value,
    length: u64,
    index: u64,
    target_index: u64,
    depth: u64,
    frame_count: usize,
    infinite_depth: bool,
}

impl Isolate {
    /// Captures flat inputs before the observable receiver length lookup.
    pub(crate) fn begin_array_flat(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.coerce_to_object(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let depth_argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let state = self.allocate_array_flat_state(PendingArrayFlat {
            receiver,
            result: undefined,
            retained: undefined,
            constructor: undefined,
            depth_argument,
            source: receiver,
            frames: Box::new([]),
            length: 0,
            index: 0,
            target_index: 0,
            depth: 1,
            frame_count: 0,
            infinite_depth: false,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_flat_state(native_site, state)?;
        let length = self.length_atom()?;
        if let Some((state, value)) = self.dispatch_array_flat_get(
            native_site,
            state,
            ArrayFlatStage::Length,
            receiver,
            length.into(),
        )? {
            self.resume_array_flat_length(native_site, state, value)?;
        }
        Ok(())
    }

    /// Routes every observable flat completion to its explicit algorithm stage.
    pub(crate) fn resume_array_flat(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        stage: ArrayFlatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_flat_state(site, state)?;
        match stage {
            ArrayFlatStage::Length => self.resume_array_flat_length(site, state, value),
            ArrayFlatStage::SpeciesConstructor => {
                self.resume_array_flat_constructor(site, state, value)
            }
            ArrayFlatStage::SpeciesValue => {
                self.finish_array_flat_species(site, state, value, true)
            }
            ArrayFlatStage::SpeciesConstruct => {
                self.finish_array_flat_construct(site, state, value)
            }
            ArrayFlatStage::SourceHas => self.resume_array_flat_source_has(site, state, value),
            ArrayFlatStage::SourceGet => self.finish_array_flat_source_get(site, state, value),
            ArrayFlatStage::ElementLength => {
                self.resume_array_flat_element_length(site, state, value)
            }
            ArrayFlatStage::Define => self.finish_array_flat_define(site, state),
        }
    }

    /// Resumes length, depth, or nested-array length conversion.
    pub(crate) fn resume_array_flat_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_flat_state(site, state)?;
        match consumer {
            ConversionConsumer::ArrayFlatLength => {
                self.finish_array_flat_length(site, state, value)
            }
            ConversionConsumer::ArrayFlatDepth => self.finish_array_flat_depth(site, state, value),
            ConversionConsumer::ArrayFlatElementLength => {
                self.finish_array_flat_element_length(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Dispatches ToLength for the observed receiver length.
    fn resume_array_flat_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayFlatLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_flat_length(site, state, value)
    }

    /// Stores source length, then performs the optional depth conversion.
    fn finish_array_flat_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(value)?;
        let length = self.array_flat_to_length(number)?;
        self.update_array_flat_scalars(state, |pending| pending.length = length)?;
        let depth = self.array_flat_snapshot(state)?.depth_argument;
        if depth.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_array_flat_depth(site, state, Value::from_i32(1));
        }
        if self.is_object_value(depth) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayFlatDepth,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                depth,
                site.call_site,
            );
        }
        self.finish_array_flat_depth(site, state, depth)
    }

    /// Normalizes ToIntegerOrInfinity(depth), installs frame backing, and starts species lookup.
    fn finish_array_flat_depth(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let converted = self.convert_to_number(value)?;
        let number = numeric_value(converted)
            .ok_or(ExecutionError::UnsupportedNumberConversion(converted))?;
        let infinite_depth = number == f64::INFINITY;
        let depth = if number.is_nan() || number <= 0.0 {
            0
        } else if infinite_depth || number >= u64::MAX as f64 {
            u64::MAX
        } else {
            number.floor() as u64
        };
        let state = self.prepare_array_flat_frames(site, state, depth, infinite_depth)?;
        self.begin_array_flat_species(site, state)
    }

    /// Implements IsArray and observable constructor lookup for ArraySpeciesCreate(O, 0).
    fn begin_array_flat_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.array_flat_snapshot(state)?.receiver;
        if !self.is_array_value(receiver)? {
            return self.finish_array_flat_species(
                site,
                state,
                Value::from_immediate(Immediate::Undefined),
                false,
            );
        }
        let constructor = self.constructor_atom()?;
        if let Some((state, value)) = self.dispatch_array_flat_get(
            site,
            state,
            ArrayFlatStage::SpeciesConstructor,
            receiver,
            constructor.into(),
        )? {
            self.resume_array_flat_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Applies the cross-Realm intrinsic Array fallback before reading Symbol.species.
    fn resume_array_flat_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return self.finish_array_flat_species(site, state, value, false);
        }
        if self.is_constructor_value(value)? {
            let constructor_realm = self.realm_for_callable(value)?;
            if constructor_realm != self.active_realm
                && self.realm_array_constructor(constructor_realm) == Some(value)
            {
                return self.finish_array_flat_species(
                    site,
                    state,
                    Value::from_immediate(Immediate::Undefined),
                    false,
                );
            }
        }
        self.set_array_flat_value(state, |pending| &mut pending.constructor, value)?;
        let species = self
            .realm
            .well_known_symbols
            .species
            .expect("Symbol.species initializes before Array");
        let key = self.property_key(species)?;
        if let Some((state, observed)) =
            self.dispatch_array_flat_get(site, state, ArrayFlatStage::SpeciesValue, value, key)?
        {
            self.finish_array_flat_species(site, state, observed, true)?;
        }
        Ok(())
    }

    /// Creates the intrinsic empty Array or invokes a custom species constructor.
    fn finish_array_flat_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        constructor: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined)
            || (from_species && constructor.as_immediate() == Some(Immediate::Null))
        {
            self.root_array_flat_state(site, state)?;
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before flat");
            let result = self.create_array_object_with_prototype(prototype)?;
            let state =
                self.pending_array_flat_reference(self.read(site.caller_base, site.destination)?)?;
            self.set_array_flat_value(state, |pending| &mut pending.result, result)?;
            self.set_array_length_value(result, Value::from_i32(0))?;
            return self.advance_array_flat(site, state);
        }
        if constructor.as_immediate() == Some(Immediate::Null) || !self.is_object_value(constructor)
        {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_array_flat_species(site, state, constructor)
    }

    /// Calls a custom species constructor with the single zero length argument.
    fn construct_array_flat_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_flat_value(state, |pending| &mut pending.constructor, constructor)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        arguments.push(Value::from_i32(0));
        self.push_array_flat_parent(site, state, ArrayFlatStage::SpeciesConstruct, constructor)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, arguments) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_flat_reference(rooted.first())?;
        let constructor = rooted.second();
        self.push_array_flat_parent(
            site,
            state,
            ArrayFlatStage::SpeciesConstruct,
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
        let state = self.pending_array_flat_reference(rooted.first())?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_flat_construct(site, state, result)
    }

    /// Validates a custom species result before starting the flatten traversal.
    fn finish_array_flat_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        self.set_array_flat_value(state, |pending| &mut pending.result, result)?;
        self.advance_array_flat(site, state)
    }

    /// Iteratively walks the current source and restores explicit parent frames at exhaustion.
    fn advance_array_flat(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArrayFlat>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.root_array_flat_state(site, state)?;
            let snapshot = self.array_flat_snapshot(state)?;
            if snapshot.index >= snapshot.length {
                if snapshot.frame_count == 0 {
                    return self.write(site.caller_base, site.destination, snapshot.result);
                }
                state = self.pop_array_flat_frame(site, state)?;
                continue;
            }
            let index = snapshot.index;
            self.update_array_flat_scalars(state, |pending| pending.index += 1)?;
            let Some((rooted, has)) = self.dispatch_array_flat_has(
                site,
                state,
                ArrayFlatStage::SourceHas,
                snapshot.source,
                safe_integer_value(index),
            )?
            else {
                return Ok(());
            };
            state = rooted;
            if self.is_truthy_value(has)? {
                return self.dispatch_array_flat_source_get(site, state);
            }
            self.skip_array_flat_holes(state)?;
        }
    }

    /// Handles one HasProperty completion and skips only proven ordinary holes.
    fn resume_array_flat_source_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_truthy_value(value)? {
            return self.dispatch_array_flat_source_get(site, state);
        }
        self.skip_array_flat_holes(state)?;
        self.advance_array_flat(site, state)
    }

    /// Dispatches Get for the current present source property.
    fn dispatch_array_flat_source_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_flat_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.index - 1)?;
        if let Some((state, value)) = self.dispatch_array_flat_get(
            site,
            state,
            ArrayFlatStage::SourceGet,
            snapshot.source,
            key.into(),
        )? {
            self.finish_array_flat_source_get(site, state, value)?;
        }
        Ok(())
    }

    /// Selects direct output or begins observable length lookup for a nested Array.
    fn finish_array_flat_source_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_flat_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_flat_snapshot(state)?;
        if !snapshot.infinite_depth && snapshot.depth == 0 {
            return self.write_array_flat_value(site, state, value);
        }
        self.root_array_flat_state(site, state)?;
        if !self.is_array_value(value)? {
            return self.write_array_flat_value(site, state, value);
        }
        let length = self.length_atom()?;
        if let Some((state, observed)) = self.dispatch_array_flat_get(
            site,
            state,
            ArrayFlatStage::ElementLength,
            value,
            length.into(),
        )? {
            self.resume_array_flat_element_length(site, state, observed)?;
        }
        Ok(())
    }

    /// Dispatches ToLength for one nested Array's observed length.
    fn resume_array_flat_element_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayFlatElementLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_flat_element_length(site, state, value)
    }

    /// Pushes the current traversal and descends into one nested Array.
    fn finish_array_flat_element_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(value)?;
        let length = self.array_flat_to_length(number)?;
        let state = self.push_array_flat_frame(site, state)?;
        let snapshot = self.array_flat_snapshot(state)?;
        let nested = snapshot.retained;
        self.set_array_flat_value(state, |pending| &mut pending.source, nested)?;
        self.update_array_flat_scalars(state, |pending| {
            pending.length = length;
            pending.index = 0;
            if !pending.infinite_depth {
                pending.depth = pending.depth.saturating_sub(1);
            }
        })?;
        self.advance_array_flat(site, state)
    }

    /// Creates one dense target data property while retaining its value across allocation.
    fn write_array_flat_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_flat_value(state, |pending| &mut pending.retained, value)?;
        self.root_array_flat_state(site, state)?;
        let snapshot = self.array_flat_snapshot(state)?;
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
            return self.dispatch_array_flat_define(
                site,
                state,
                snapshot.result,
                key.into(),
                descriptor.into(),
            );
        }
        self.define_data_property(snapshot.result, key, descriptor)?;
        let state =
            self.pending_array_flat_reference(self.read(site.caller_base, site.destination)?)?;
        self.finish_array_flat_define(site, state)
    }

    /// Commits the target cursor only after CreateDataPropertyOrThrow succeeds.
    fn finish_array_flat_define(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayFlat>,
    ) -> Result<(), ExecutionError> {
        self.update_array_flat_scalars(state, |pending| pending.target_index += 1)?;
        self.advance_array_flat(site, state)
    }
}
