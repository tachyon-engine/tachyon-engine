//! ArraySpeciesCreate and construct boundaries shared by map and filter.

use super::*;

impl Isolate {
    /// Starts ArraySpeciesCreate after length conversion and callback validation have completed.
    pub(super) fn begin_array_output_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[FOREACH_RECEIVER];
        if !self.is_array_value(receiver)? {
            return self.finish_array_output_species(
                site,
                state,
                Value::from_immediate(Immediate::Undefined),
                false,
            );
        }
        let constructor = self.constructor_atom()?;
        let observed = self.dispatch_array_for_each_get(
            site,
            state,
            ArrayForEachStage::OutputConstructor,
            receiver,
            constructor.into(),
        )?;
        if let Some(observed) = observed {
            self.resume_array_for_each(
                site,
                state,
                ArrayForEachStage::OutputConstructor,
                observed,
                receiver,
            )?;
        }
        Ok(())
    }

    /// Selects the intrinsic Array fallback or constructs the observed species length.
    pub(super) fn finish_array_output_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined)
            || (from_species && constructor.as_immediate() == Some(Immediate::Null))
        {
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before Array output methods");
            let result = self.create_array_object_with_prototype(prototype)?;
            let rooted_state = self.read(site.caller_base, site.destination)?;
            let state = self.native_call_state_reference(rooted_state)?;
            let (output, kind) = self
                .array_output_state(state)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            self.set_array_for_each_value(output, OUTPUT_RESULT, result)?;
            let length = exact_nonnegative_integer(
                self.native_call_state_snapshot(state)?.values[FOREACH_LENGTH],
            )?;
            self.set_array_length_value(
                result,
                safe_integer_value(kind.construction_length(length)),
            )?;
            return self.advance_array_for_each(site, state);
        }
        if constructor.as_immediate() == Some(Immediate::Null) || !self.is_object_value(constructor)
        {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_array_output_species(site, state, constructor)
    }

    /// Roots a custom species constructor and its length prefix across Construct.
    fn construct_array_output_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let rooted_state = self.read(site.caller_base, site.destination)?;
        let state = self.native_call_state_reference(rooted_state)?;
        let (output, kind) = self
            .array_output_state(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_array_for_each_value(output, OUTPUT_CONSTRUCTOR, constructor)?;
        self.push_array_for_each_parent(
            site,
            state,
            ArrayForEachStage::OutputConstruct,
            constructor,
        )?;
        let length = exact_nonnegative_integer(
            self.native_call_state_snapshot(state)?.values[FOREACH_LENGTH],
        )?;
        let prefix = match self.create_apply_argument_prefix(
            constructor,
            undefined,
            vec![safe_integer_value(kind.construction_length(length))],
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        self.pop_native_continuation()?;
        self.push_array_for_each_parent(
            site,
            state,
            ArrayForEachStage::OutputConstruct,
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
        self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_array_output_construct(site, state, result)
    }

    /// Publishes a custom species result only after Construct has returned an object.
    pub(super) fn finish_array_output_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        let (output, _) = self
            .array_output_state(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_array_for_each_value(output, OUTPUT_RESULT, result)?;
        self.advance_array_for_each(site, state)
    }
}
