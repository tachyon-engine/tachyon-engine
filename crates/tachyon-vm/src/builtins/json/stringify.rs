//! GC-owned iterative JSON serialization and observable callback dispatch.

use core::mem::size_of;

use tachyon_gc::{AllocationSpace, GcExternalMemory, GcRef, Trace, Tracer};

use super::*;
use crate::{regexp_exec::regexp_to_length, tuning};

mod property_list;
mod state;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum JsonContainerKind {
    Array,
    Object,
}

#[derive(Clone, Debug)]
struct JsonFrame {
    container: Value,
    keys: Value,
    index: u64,
    length: u64,
    wrote_property: bool,
    descriptor_checks: bool,
    kind: JsonContainerKind,
}

impl Trace for JsonFrame {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.container.trace(tracer);
        self.keys.trace(tracer);
    }
}

/// The sole owner of every JavaScript edge and native buffer retained across JSON callbacks.
#[derive(Debug)]
pub(crate) struct PendingJsonStringify {
    replacer: Value,
    property_list: Value,
    property_list_source: Value,
    holder: Value,
    key: Value,
    value: Value,
    temporary: Value,
    space: Value,
    property_list_index: u64,
    property_list_length: u64,
    property_list_count: u64,
    indentation: JsonIndentation,
    output: Vec<u16>,
    frames: Vec<JsonFrame>,
}

impl Trace for PendingJsonStringify {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.replacer.trace(tracer);
        self.property_list.trace(tracer);
        self.property_list_source.trace(tracer);
        self.holder.trace(tracer);
        self.key.trace(tracer);
        self.value.trace(tracer);
        self.temporary.trace(tracer);
        self.space.trace(tracer);
        self.frames.trace(tracer);
    }
}

impl GcExternalMemory for PendingJsonStringify {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.output
            .capacity()
            .saturating_mul(size_of::<u16>())
            .saturating_add(
                self.frames
                    .capacity()
                    .saturating_mul(size_of::<JsonFrame>()),
            )
    }
}

#[derive(Clone, Copy)]
struct JsonSnapshot {
    replacer: Value,
    property_list: Value,
    property_list_source: Value,
    holder: Value,
    key: Value,
    value: Value,
    indentation: JsonIndentation,
    space: Value,
    property_list_index: u64,
    property_list_length: u64,
    property_list_count: u64,
    frame_depth: usize,
}

#[derive(Clone, Copy)]
struct JsonFrameSnapshot {
    container: Value,
    index: u64,
    length: u64,
    wrote_property: bool,
    descriptor_checks: bool,
    kind: JsonContainerKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonWrapperKind {
    Number,
    String,
    Boolean,
    BigInt,
}

#[derive(Clone, Copy, Debug)]
enum JsonPropertyReadDispatch {
    Suspended,
    Returned(Value),
}

impl Isolate {
    /// Initializes the wrapper holder and publishes all serialization state before any callback.
    pub(crate) fn begin_json_stringify(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let replacer = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let replacer_is_callable = self.is_callable_value(replacer)?;
        let replacer_is_array = !replacer_is_callable && self.is_array_value(replacer)?;
        let wrapper = self.create_ordinary_object_with_prototype(
            self.realm
                .object_prototype
                .expect("Object prototype initializes before JSON"),
        )?;
        self.write(site.caller_base, site.destination, wrapper)?;
        let empty = self.intern_intrinsic_name(b"")?;
        let input = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.set_own_data_property(wrapper, empty, input)?;
        let empty_key = self.atom_string_value(empty)?;
        let wrapper = self.read(site.caller_base, site.destination)?;
        let replacer = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let space = self
            .call_argument(site, 2)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let replacer_function = if replacer_is_callable {
            replacer
        } else {
            Value::from_immediate(Immediate::Undefined)
        };
        let indentation = if self.json_boxed_space_consumer(space).is_some() {
            JsonIndentation::compact()
        } else {
            self.json_primitive_indentation(space)?
        };
        let mut output = Vec::new();
        output
            .try_reserve_exact(tuning::json::INITIAL_OUTPUT_UNITS)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(tuning::json::INITIAL_FRAME_CAPACITY)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let state = self.allocate_json_stringify_state(PendingJsonStringify {
            replacer: replacer_function,
            property_list: Value::from_immediate(Immediate::Undefined),
            property_list_source: if replacer_is_array {
                replacer
            } else {
                Value::from_immediate(Immediate::Undefined)
            },
            holder: wrapper,
            key: empty_key,
            value: Value::from_immediate(Immediate::Undefined),
            temporary: Value::from_immediate(Immediate::Undefined),
            space,
            property_list_index: 0,
            property_list_length: 0,
            property_list_count: 0,
            indentation,
            output,
            frames,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_json_stringify_state(native_site, state)?;
        if replacer_is_array {
            let property_list = self.create_array_object_with_prototype(
                self.realm
                    .array_prototype
                    .expect("Array prototype initializes before JSON"),
            )?;
            let state = self.refresh_json_state(native_site)?;
            self.set_json_property_list(state, property_list)?;
            return self.begin_json_property_list_length(native_site, state);
        }
        self.begin_json_after_property_list(native_site, state)
    }

    /// Resumes initialization after boxed Number/String `space` conversion.
    pub(crate) fn resume_json_space_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let indentation = self.json_primitive_indentation(primitive)?;
        self.set_json_indentation(state, indentation)?;
        self.root_json_stringify_state(site, state)?;
        self.begin_json_property_get(site, state)
    }

    /// Resumes boxed Number/String value conversion without losing the active container graph.
    pub(crate) fn resume_json_value_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.set_json_value(state, primitive)?;
        self.root_json_stringify_state(site, state)?;
        self.serialize_json_transformed_value(site, state)
    }

    /// Resumes ToLength for a Proxy Array's observable length result.
    pub(crate) fn resume_json_array_length_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let length = regexp_to_length(self.convert_to_number(primitive)?)?;
        self.set_json_top_frame_length(state, length)?;
        self.root_json_stringify_state(site, state)?;
        self.advance_json_container(site, state)
    }

    /// Continues one observable Get/call or Proxy ownKeys result.
    pub(crate) fn resume_json_stringify(
        &mut self,
        continuation: NativeContinuation,
        stage: JsonStringifyStage,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_json_stringify_reference(continuation.first())?;
        let site = continuation.site();
        self.root_json_stringify_state(site, state)?;
        match stage {
            JsonStringifyStage::ValueGet => {
                self.set_json_value(state, result)?;
                self.begin_json_to_json_get(site, state)
            }
            JsonStringifyStage::ToJsonGet => {
                if self.is_callable_value(result)? {
                    self.set_json_temporary(state, result)?;
                    self.dispatch_json_callback(site, state, JsonStringifyStage::ToJsonCall, true)
                } else {
                    self.begin_json_replacer_call(site, state)
                }
            }
            JsonStringifyStage::ToJsonCall => {
                self.set_json_value(state, result)?;
                self.begin_json_replacer_call(site, state)
            }
            JsonStringifyStage::ReplacerCall => {
                self.set_json_value(state, result)?;
                self.serialize_json_transformed_value(site, state)
            }
            JsonStringifyStage::ReplacerLengthGet => {
                if self.is_object_value(result) {
                    return self.dispatch_object_primitive_conversion(
                        ConversionConsumer::JsonStringifyPropertyListLength,
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                        result,
                        site.call_site,
                    );
                }
                self.resume_json_property_list_length_conversion(site, state, result)
            }
            JsonStringifyStage::ReplacerElementGet => {
                self.begin_json_property_list_element(site, state, result)
            }
            JsonStringifyStage::ArrayLengthGet => {
                if self.is_object_value(result) {
                    return self.dispatch_object_primitive_conversion(
                        ConversionConsumer::JsonStringifyArrayLength,
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                        result,
                        site.call_site,
                    );
                }
                self.resume_json_array_length_conversion(site, state, result)
            }
            JsonStringifyStage::ObjectKeys => {
                let length = self.json_key_array_length(result)?;
                self.set_json_top_frame_keys(state, result, length)?;
                self.set_json_top_frame_descriptor_checks(state)?;
                self.advance_json_container(site, state)
            }
            JsonStringifyStage::ObjectDescriptor => {
                if result.as_immediate() == Some(Immediate::Undefined)
                    || self.parse_property_descriptor(result)?.enumerable() != Some(true)
                {
                    self.advance_json_top_frame_index(state)?;
                    return self.advance_json_container(site, state);
                }
                self.begin_json_current_frame_property(site, state)
            }
        }
    }

    /// Performs holder[key] through ordinary accessors and Proxy `[[Get]]`.
    fn begin_json_property_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.json_snapshot(state)?;
        let key = self.property_key(snapshot.key)?;
        self.dispatch_json_property_read(
            site,
            state,
            JsonStringifyStage::ValueGet,
            snapshot.holder,
            snapshot.holder,
            key,
        )
    }

    /// Observes `toJSON` only for Objects and BigInt primitives, then preserves the original key.
    fn begin_json_to_json_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        let value = self.json_snapshot(state)?.value;
        if !self.is_object_value(value) && !self.is_bigint_value(value) {
            return self.begin_json_replacer_call(site, state);
        }
        let to_json = self.intern_intrinsic_name(b"toJSON")?;
        self.dispatch_json_property_read(
            site,
            state,
            JsonStringifyStage::ToJsonGet,
            value,
            value,
            to_json.into(),
        )
    }

    /// Calls the replacer with holder as `this`, after `toJSON` has completed.
    fn begin_json_replacer_call(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.json_snapshot(state)?;
        if snapshot.replacer.as_immediate() == Some(Immediate::Undefined) {
            return self.serialize_json_transformed_value(site, state);
        }
        self.set_json_temporary(state, snapshot.replacer)?;
        self.dispatch_json_callback(site, state, JsonStringifyStage::ReplacerCall, false)
    }

    /// Applies wrapper unboxing and selects primitive, omission, or container serialization.
    fn serialize_json_transformed_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        let mut value = self.json_snapshot(state)?.value;
        if let Some(kind) = self.json_wrapper_kind(value) {
            match kind {
                JsonWrapperKind::Number | JsonWrapperKind::String => {
                    let consumer = if kind == JsonWrapperKind::Number {
                        ConversionConsumer::JsonStringifyNumberValue
                    } else {
                        ConversionConsumer::JsonStringifyStringValue
                    };
                    return self.dispatch_object_primitive_conversion(
                        consumer,
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                        value,
                        site.call_site,
                    );
                }
                JsonWrapperKind::Boolean => value = self.this_boolean_value(value)?,
                JsonWrapperKind::BigInt => value = self.this_bigint_value(value)?,
            }
            self.set_json_value(state, value)?;
        }
        if self.is_bigint_value(value) {
            return Err(ExecutionError::UnsupportedBigIntConversion(value));
        }
        if let Some(immediate) = value.as_immediate() {
            return match immediate {
                Immediate::Null => self.finish_json_primitive(site, state, Some(b"null")),
                Immediate::True => self.finish_json_primitive(site, state, Some(b"true")),
                Immediate::False => self.finish_json_primitive(site, state, Some(b"false")),
                Immediate::Undefined => self.finish_json_primitive(site, state, None),
                Immediate::Hole | Immediate::Uninitialized => Err(ExecutionError::InvalidJsonText),
            };
        }
        if let Some(number) = numeric_value(value) {
            let mut units = Vec::new();
            if number.is_finite() {
                self.append_primitive_string_units(value, &mut units)?;
            } else {
                units.extend(b"null".iter().copied().map(u16::from));
            }
            return self.finish_json_units(site, state, Some(&units));
        }
        if self.json_is_string(value) {
            let mut quoted = Vec::new();
            self.json_quote_string(value, &mut quoted)?;
            return self.finish_json_units(site, state, Some(&quoted));
        }
        if self.json_is_symbol(value) || self.is_callable_value(value)? {
            return self.finish_json_primitive(site, state, None);
        }
        if !self.is_object_value(value) {
            return self.finish_json_primitive(site, state, None);
        }
        self.enter_json_container(site, state, value)
    }

    /// Pushes one Array/Object frame after checking the active ancestor identities.
    fn enter_json_container(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        container: Value,
    ) -> Result<(), ExecutionError> {
        if self.json_contains_container(state, container)? {
            return Err(ExecutionError::InvalidJsonCircularStructure);
        }
        let depth = self.json_snapshot(state)?.frame_depth;
        if depth >= MAX_JSON_DEPTH as usize {
            return Err(ExecutionError::JsonSerializationDepthExceeded);
        }
        let mut state = self.append_json_property_prefix(site, state)?;
        let container = self.json_snapshot(state)?.value;
        let kind = if self.is_array_value(container)? {
            JsonContainerKind::Array
        } else {
            JsonContainerKind::Object
        };
        state = self.append_json_ascii(
            site,
            state,
            if kind == JsonContainerKind::Array {
                b"["
            } else {
                b"{"
            },
        )?;
        state = self.push_json_frame(site, state, kind)?;
        let container = self.json_snapshot(state)?.value;
        self.root_json_stringify_state(site, state)?;
        match kind {
            JsonContainerKind::Array => {
                let length = self.length_atom()?;
                self.dispatch_json_property_read(
                    site,
                    state,
                    JsonStringifyStage::ArrayLengthGet,
                    container,
                    container,
                    length.into(),
                )
            }
            JsonContainerKind::Object
                if self.json_snapshot(state)?.property_list.as_immediate()
                    != Some(Immediate::Undefined) =>
            {
                let snapshot = self.json_snapshot(state)?;
                self.set_json_top_frame_keys(
                    state,
                    snapshot.property_list,
                    snapshot.property_list_count,
                )?;
                self.advance_json_container(site, state)
            }
            JsonContainerKind::Object if self.is_proxy_value(container) => {
                self.dispatch_json_object_keys(site, state, container)
            }
            JsonContainerKind::Object => {
                let keys = self.json_ordinary_enumerable_keys(container)?;
                let length = keys.len() as u64;
                let keys = self.json_atom_key_array(site, keys)?;
                let state = self.refresh_json_state(site)?;
                self.set_json_top_frame_keys(state, keys, length)?;
                self.advance_json_container(site, state)
            }
        }
    }

    /// Iteratively selects the next member or closes the current container.
    fn advance_json_container(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        loop {
            let frame = self
                .json_top_frame_snapshot(state)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            if frame.index >= frame.length {
                return self.close_json_container(site, state, frame);
            }
            let atom = match frame.kind {
                JsonContainerKind::Array => self.safe_integer_property_atom(frame.index)?,
                JsonContainerKind::Object => self
                    .json_top_frame_key(state, frame.index as usize)?
                    .ok_or(ExecutionError::MissingNativeContinuation)?,
            };
            if frame.descriptor_checks {
                return self.dispatch_json_object_descriptor(site, state, frame.container, atom);
            }
            if frame.kind != JsonContainerKind::Object
                || self.json_snapshot(state)?.replacer.as_immediate() != Some(Immediate::Undefined)
            {
                return self.begin_json_current_frame_property(site, state);
            }
            let key = self.atom_string_value(atom)?;
            state = self.refresh_json_state(site)?;
            let container = self
                .json_top_frame_snapshot(state)?
                .ok_or(ExecutionError::MissingNativeContinuation)?
                .container;
            self.set_json_current_property(state, container, key)?;
            self.root_json_stringify_state(site, state)?;
            let dispatch = self.dispatch_json_property_read_once(
                site,
                state,
                JsonStringifyStage::ValueGet,
                container,
                container,
                atom.into(),
            )?;
            let JsonPropertyReadDispatch::Returned(result) = dispatch else {
                return Ok(());
            };
            let continuation = self.pop_native_continuation()?;
            state = self.pending_json_stringify_reference(continuation.first())?;
            self.set_json_value(state, result)?;
            self.root_json_stringify_state(site, state)?;
            if result.as_immediate() != Some(Immediate::Undefined) {
                return self.begin_json_to_json_get(site, state);
            }
            self.advance_json_top_frame_index(state)?;
        }
    }

    /// Publishes the current frame's holder/key pair before its observable value Get.
    fn begin_json_current_frame_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        let frame = self
            .json_top_frame_snapshot(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let atom = match frame.kind {
            JsonContainerKind::Array => self.safe_integer_property_atom(frame.index)?,
            JsonContainerKind::Object => self
                .json_top_frame_key(state, frame.index as usize)?
                .ok_or(ExecutionError::MissingNativeContinuation)?,
        };
        let key = self.atom_string_value(atom)?;
        let state = self.refresh_json_state(site)?;
        let container = self
            .json_top_frame_snapshot(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?
            .container;
        self.set_json_current_property(state, container, key)?;
        self.root_json_stringify_state(site, state)?;
        self.begin_json_property_get(site, state)
    }

    /// Emits the closing indentation/token, pops the child, and advances its parent.
    fn close_json_container(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingJsonStringify>,
        frame: JsonFrameSnapshot,
    ) -> Result<(), ExecutionError> {
        let has_content = match frame.kind {
            JsonContainerKind::Array => frame.length != 0,
            JsonContainerKind::Object => frame.wrote_property,
        };
        if has_content {
            let snapshot = self.json_snapshot(state)?;
            if !snapshot.indentation.is_compact() {
                state = self.append_json_line_indent(
                    site,
                    state,
                    snapshot.frame_depth.saturating_sub(1),
                )?;
            }
        }
        state = self.append_json_ascii(
            site,
            state,
            if frame.kind == JsonContainerKind::Array {
                b"]"
            } else {
                b"}"
            },
        )?;
        self.pop_json_frame(state)?;
        self.complete_json_property(site, state, true)
    }

    fn finish_json_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        ascii: Option<&[u8]>,
    ) -> Result<(), ExecutionError> {
        let units = ascii.map(|bytes| bytes.iter().copied().map(u16::from).collect::<Vec<_>>());
        self.finish_json_units(site, state, units.as_deref())
    }

    /// Applies Array null substitution/Object omission before committing one primitive result.
    fn finish_json_units(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingJsonStringify>,
        units: Option<&[u16]>,
    ) -> Result<(), ExecutionError> {
        let parent = self.json_top_frame_snapshot(state)?;
        const NULL_UNITS: [u16; 4] = [b'n' as u16, b'u' as u16, b'l' as u16, b'l' as u16];
        let units = match (parent.map(|frame| frame.kind), units) {
            (Some(JsonContainerKind::Array), None) => Some(NULL_UNITS.as_slice()),
            (_, units) => units,
        };
        if let Some(units) = units {
            state = self.append_json_property_prefix(site, state)?;
            state = self.append_json_units(site, state, units)?;
        }
        self.complete_json_property(site, state, units.is_some())
    }

    /// Finalizes the root or advances the parent cursor after one property completes.
    fn complete_json_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        serialized: bool,
    ) -> Result<(), ExecutionError> {
        if self.json_snapshot(state)?.frame_depth == 0 {
            if !serialized {
                return self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            let output = self.copy_json_output(state)?;
            let result = self.allocate_runtime_string(
                JsString::try_from_owned_code_units(output)
                    .map_err(ExecutionError::ConstantString)?,
            )?;
            return self.write(site.caller_base, site.destination, result);
        }
        self.advance_json_top_frame_index(state)?;
        self.root_json_stringify_state(site, state)?;
        self.advance_json_container(site, state)
    }

    /// Adds the comma/newline/key/colon prefix exactly when a value is known to serialize.
    fn append_json_property_prefix(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let snapshot = self.json_snapshot(state)?;
        let Some(frame) = self.json_top_frame_snapshot(state)? else {
            return Ok(state);
        };
        let mut prefix = Vec::new();
        let has_previous = match frame.kind {
            JsonContainerKind::Array => frame.index != 0,
            JsonContainerKind::Object => frame.wrote_property,
        };
        if has_previous {
            prefix.push(u16::from(b','));
        }
        if !snapshot.indentation.is_compact() {
            snapshot
                .indentation
                .append_line_indent(snapshot.frame_depth, &mut prefix)?;
        }
        if frame.kind == JsonContainerKind::Object {
            self.json_quote_string(snapshot.key, &mut prefix)?;
            prefix.push(u16::from(b':'));
            if !snapshot.indentation.is_compact() {
                prefix.push(u16::from(b' '));
            }
            self.set_json_top_frame_wrote(state)?;
        }
        self.append_json_units(site, state, &prefix)
    }

    /// Runs one Proxy/accessor-aware Get while keeping the JSON state as its typed parent.
    fn dispatch_json_property_read(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        stage: JsonStringifyStage,
        target: Value,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        match self.dispatch_json_property_read_once(site, state, stage, target, receiver, key)? {
            JsonPropertyReadDispatch::Suspended => Ok(()),
            JsonPropertyReadDispatch::Returned(result) => {
                let continuation = self.pop_native_continuation()?;
                self.resume_json_stringify(continuation, stage, result)
            }
        }
    }

    /// Performs one Get and distinguishes an immediate result from a published JS frame.
    fn dispatch_json_property_read_once(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        stage: JsonStringifyStage,
        target: Value,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<JsonPropertyReadDispatch, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::json_stringify(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, target, receiver, key) {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return Ok(JsonPropertyReadDispatch::Suspended);
        }
        let result = self.read(site.caller_base, site.destination)?;
        Ok(JsonPropertyReadDispatch::Returned(result))
    }

    /// Calls `toJSON` or the replacer with an exact immutable argument prefix.
    fn dispatch_json_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        stage: JsonStringifyStage,
        to_json: bool,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.json_snapshot(state)?;
        let callee = self.json_temporary(state)?;
        let (this_value, arguments) = if to_json {
            (snapshot.value, vec![snapshot.key])
        } else {
            (snapshot.holder, vec![snapshot.key, snapshot.value])
        };
        let argument_count = arguments.len() as u32;
        let prefix = self.create_apply_argument_prefix(callee, this_value, arguments)?;
        let state = self.refresh_json_state(site)?;
        let snapshot = self.json_snapshot(state)?;
        let callee = self.json_temporary(state)?;
        let this_value = if to_json {
            snapshot.value
        } else {
            snapshot.holder
        };
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::json_stringify(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Self::completion_stack_error)?;
        let call_result = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: argument_count,
            argument_count,
            this_value,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        });
        if let Err(error) = call_result {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            if self.fiber.frames.len() != frame_depth {
                let frame = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("a suspended JSON callback publishes its frame");
                frame.return_register = None;
                frame.return_continuation = true;
            }
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.resume_json_stringify(continuation, stage, result)
    }

    /// Requests the existing complete Proxy Object.keys protocol under a JSON parent sentinel.
    fn dispatch_json_object_keys(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        object: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::json_stringify(
                site,
                JsonStringifyStage::ObjectKeys,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Self::completion_stack_error)?;
        let outcome = self.dispatch_proxy_own_keys(site, object, ProxyOwnKeysMode::Names);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.resume_json_stringify(continuation, JsonStringifyStage::ObjectKeys, result)
    }

    /// Queries one Proxy own-property descriptor before the corresponding value Get.
    fn dispatch_json_object_descriptor(
        &mut self,
        site: NativeContinuationSite,
        _state: GcRef<PendingJsonStringify>,
        _object: Value,
        atom: AtomId,
    ) -> Result<(), ExecutionError> {
        let key = self.atom_string_value(atom)?;
        let state = self.refresh_json_state(site)?;
        let object = self
            .json_top_frame_snapshot(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?
            .container;
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::json_stringify(
                site,
                JsonStringifyStage::ObjectDescriptor,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(Self::completion_stack_error)?;
        let outcome = self.dispatch_proxy_get_own(site, object, key, ProxyGetOwnMode::Descriptor);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.resume_json_stringify(continuation, JsonStringifyStage::ObjectDescriptor, result)
    }

    fn json_primitive_indentation(
        &mut self,
        space: Value,
    ) -> Result<JsonIndentation, ExecutionError> {
        if let Some(number) = numeric_value(space) {
            let integer = if number.is_nan() || number == 0.0 {
                0.0
            } else {
                number.trunc()
            };
            let length = integer.clamp(0.0, MAX_JSON_GAP_UNITS as f64) as usize;
            return Ok(JsonIndentation::spaces(length));
        }
        let mut indentation = JsonIndentation::compact();
        if self.json_is_string(space) {
            let mut units = Vec::new();
            self.append_primitive_string_units(space, &mut units)?;
            let length = units.len().min(MAX_JSON_GAP_UNITS);
            indentation.gap[..length].copy_from_slice(&units[..length]);
            indentation.gap_length = length;
        }
        Ok(indentation)
    }

    fn json_boxed_space_consumer(&self, space: Value) -> Option<ConversionConsumer> {
        match self.json_wrapper_kind(space) {
            Some(JsonWrapperKind::Number) => Some(ConversionConsumer::JsonStringifyNumberSpace),
            Some(JsonWrapperKind::String) => Some(ConversionConsumer::JsonStringifyStringSpace),
            _ => None,
        }
    }

    fn json_wrapper_kind(&self, value: Value) -> Option<JsonWrapperKind> {
        let raw = value.as_heap_ref()?;
        if self
            .heap
            .checked_reference(raw, self.types.number_object)
            .is_ok()
        {
            return Some(JsonWrapperKind::Number);
        }
        if self
            .heap
            .checked_reference(raw, self.types.string_object)
            .is_ok()
        {
            return Some(JsonWrapperKind::String);
        }
        if self
            .heap
            .checked_reference(raw, self.types.boolean_object)
            .is_ok()
        {
            return Some(JsonWrapperKind::Boolean);
        }
        self.heap
            .checked_reference(raw, self.types.bigint_object)
            .is_ok()
            .then_some(JsonWrapperKind::BigInt)
    }
}
