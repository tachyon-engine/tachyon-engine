use super::*;

impl Isolate {
    /// Resumes ToLength for the observable replacer-array length.
    pub(crate) fn resume_json_property_list_length_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let length = regexp_to_length(self.convert_to_number(primitive)?)?;
        self.set_json_property_list_length(state, length)?;
        self.root_json_stringify_state(site, state)?;
        self.advance_json_property_list(site, state)
    }

    /// Resumes boxed String/Number property-list entry conversion.
    pub(crate) fn resume_json_property_list_element_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_json_stringify_state(site, state)?;
        self.finish_json_property_list_element(site, state, Some(primitive))
    }

    /// Applies space only after the replacer property list has been completely observed.
    pub(super) fn begin_json_after_property_list(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        let space = self.json_snapshot(state)?.space;
        if let Some(consumer) = self.json_boxed_space_consumer(space) {
            return self.dispatch_object_primitive_conversion(
                consumer,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                space,
                site.call_site,
            );
        }
        let indentation = self.json_primitive_indentation(space)?;
        self.set_json_indentation(state, indentation)?;
        self.root_json_stringify_state(site, state)?;
        self.begin_json_property_get(site, state)
    }

    /// Starts the observable length Get for a replacer Array or Proxy-to-Array.
    pub(super) fn begin_json_property_list_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        let source = self.json_snapshot(state)?.property_list_source;
        let length = self.length_atom()?;
        self.dispatch_json_property_read(
            site,
            state,
            JsonStringifyStage::ReplacerLengthGet,
            source,
            source,
            length.into(),
        )
    }

    /// Iterates replacer entries through observable indexed Gets without Rust recursion.
    pub(super) fn advance_json_property_list(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.json_snapshot(state)?;
            if snapshot.property_list_index >= snapshot.property_list_length {
                return self.begin_json_after_property_list(site, state);
            }
            let key = self.safe_integer_property_atom(snapshot.property_list_index)?;
            let dispatch = self.dispatch_json_property_read_once(
                site,
                state,
                JsonStringifyStage::ReplacerElementGet,
                snapshot.property_list_source,
                snapshot.property_list_source,
                key.into(),
            )?;
            let JsonPropertyReadDispatch::Returned(value) = dispatch else {
                return Ok(());
            };
            let continuation = self.pop_native_continuation()?;
            state = self.pending_json_stringify_reference(continuation.first())?;
            self.set_json_temporary(state, value)?;
            self.root_json_stringify_state(site, state)?;
            let value = self.json_temporary(state)?;
            if let Some(kind) = self.json_wrapper_kind(value)
                && matches!(kind, JsonWrapperKind::Number | JsonWrapperKind::String)
            {
                return self.begin_json_property_list_element(site, state, value);
            }
            let accepted =
                (numeric_value(value).is_some() || self.json_is_string(value)).then_some(value);
            state = self.commit_json_property_list_element(site, state, accepted)?;
        }
    }

    /// Converts, deduplicates, and appends one replacer property name.
    pub(super) fn begin_json_property_list_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if let Some(kind) = self.json_wrapper_kind(value)
            && matches!(kind, JsonWrapperKind::Number | JsonWrapperKind::String)
        {
            let consumer = ConversionConsumer::JsonStringifyPropertyListString;
            return self.dispatch_object_primitive_conversion(
                consumer,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        let accepted =
            (numeric_value(value).is_some() || self.json_is_string(value)).then_some(value);
        self.finish_json_property_list_element(site, state, accepted)
    }

    /// Commits one accepted unique property name, then advances the source cursor.
    pub(super) fn finish_json_property_list_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        value: Option<Value>,
    ) -> Result<(), ExecutionError> {
        let state = self.commit_json_property_list_element(site, state, value)?;
        self.advance_json_property_list(site, state)
    }

    /// Records one element and returns the refreshed state without driving the next entry.
    fn commit_json_property_list_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        value: Option<Value>,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        if let Some(value) = value {
            let atom = self.property_key_atom(value)?;
            if !self.json_property_list_contains(state, atom)? {
                self.append_json_property_list_atom(site, state, atom)?;
            }
        }
        let state = self.refresh_json_state(site)?;
        self.advance_json_property_list_index(state)?;
        self.root_json_stringify_state(site, state)?;
        Ok(state)
    }
}
