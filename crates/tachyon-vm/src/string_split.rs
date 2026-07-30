//! Resumable String.prototype.split dispatch and primitive-string splitting.

use super::*;

const SPLIT_RECEIVER: usize = 0;
const SPLIT_LIMIT: usize = 1;
const SPLIT_SEPARATOR: usize = 2;
const SPLIT_STRING: usize = 3;
const SPLIT_SEPARATOR_STRING: usize = 4;
const UINT32_MODULUS: f64 = 4_294_967_296.0;

struct StringSplitRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for StringSplitRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts String.prototype.split, preserving GetMethod before receiver conversion.
    pub(crate) fn begin_string_split(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        if matches!(
            receiver.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let separator = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let limit = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if !self.is_object_value(receiver)
            && !self.is_object_value(separator)
            && !self.is_object_value(limit)
        {
            return self.finish_primitive_string_split(native_site, receiver, separator, limit);
        }
        let state = self.allocate_string_split_state(receiver, separator, limit)?;
        if self.is_object_value(separator) {
            self.begin_string_splitter_lookup(native_site, state)
        } else {
            self.begin_string_split_receiver_conversion(native_site, state)
        }
    }

    /// Resumes either the custom @@split lookup or the receiver-preserving splitter call.
    pub(crate) fn resume_string_split(
        &mut self,
        continuation: NativeContinuation,
        stage: StringSplitStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            StringSplitStage::SplitterGet if is_nullish(value) => {
                self.begin_string_split_receiver_conversion(continuation.site(), state)
            }
            StringSplitStage::SplitterGet => {
                self.resolve_function_object(value)?;
                let separator = self.native_call_state_snapshot(state)?.values[SPLIT_SEPARATOR];
                self.dispatch_property_callback(
                    NativeContinuation::string_split(
                        continuation.site(),
                        StringSplitStage::SplitterCall,
                        Value::from_heap_ref(state.raw()),
                        separator,
                    ),
                    value,
                )?;
                Ok(())
            }
            StringSplitStage::SplitterCall => self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                value,
            ),
        }
    }

    /// Continues one object-to-primitive conversion owned by the split state machine.
    pub(crate) fn resume_string_split_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::StringSplitReceiver => {
                let string = self.string_split_to_string(primitive)?;
                self.update_string_split_value(state, SPLIT_STRING, string)?;
                self.begin_string_split_limit_conversion(site, state)
            }
            ConversionConsumer::StringSplitLimit => {
                let limit = self.string_split_to_uint32(primitive)?;
                self.update_string_split_value(
                    state,
                    SPLIT_LIMIT,
                    Value::from_f64(f64::from(limit)),
                )?;
                self.begin_string_split_separator_conversion(site, state)
            }
            ConversionConsumer::StringSplitSeparator => {
                let separator = self.string_split_to_string(primitive)?;
                self.update_string_split_value(state, SPLIT_SEPARATOR_STRING, separator)?;
                self.finish_string_split_state(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Looks up separator[Symbol.split] through ordinary accessors and Proxy [[Get]].
    fn begin_string_splitter_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let separator = self.native_call_state_snapshot(state)?.values[SPLIT_SEPARATOR];
        let symbol = self
            .realm
            .well_known_symbols
            .split
            .expect("Symbol.split initializes before String.prototype.split");
        let key = self.property_key(symbol)?;
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::string_split(
                site,
                StringSplitStage::SplitterGet,
                Value::from_heap_ref(state.raw()),
                separator,
            ))
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, separator, separator, key)
        {
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
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_string_split(continuation, StringSplitStage::SplitterGet, value)
    }

    /// Converts the receiver to String, suspending only when JavaScript conversion code runs.
    fn begin_string_split_receiver_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[SPLIT_RECEIVER];
        if self.is_object_value(receiver) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringSplitReceiver,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                receiver,
                site.call_site,
            );
        }
        let string = self.string_split_to_string(receiver)?;
        self.update_string_split_value(state, SPLIT_STRING, string)?;
        self.begin_string_split_limit_conversion(site, state)
    }

    /// Applies ToUint32 to the limit after receiver conversion has completed.
    fn begin_string_split_limit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let limit = self.native_call_state_snapshot(state)?.values[SPLIT_LIMIT];
        if limit.as_immediate() == Some(Immediate::Undefined) {
            self.update_string_split_value(
                state,
                SPLIT_LIMIT,
                Value::from_f64(f64::from(u32::MAX)),
            )?;
            return self.begin_string_split_separator_conversion(site, state);
        }
        if self.is_object_value(limit) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringSplitLimit,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                limit,
                site.call_site,
            );
        }
        let limit = self.string_split_to_uint32(limit)?;
        self.update_string_split_value(state, SPLIT_LIMIT, Value::from_f64(f64::from(limit)))?;
        self.begin_string_split_separator_conversion(site, state)
    }

    /// Converts a non-undefined separator before the normalized-limit early return.
    fn begin_string_split_separator_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        if pending.values[SPLIT_SEPARATOR].as_immediate() == Some(Immediate::Undefined) {
            return self.finish_string_split_state(site, state);
        }
        let separator = pending.values[SPLIT_SEPARATOR];
        if self.is_object_value(separator) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringSplitSeparator,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                separator,
                site.call_site,
            );
        }
        let separator = self.string_split_to_string(separator)?;
        self.update_string_split_value(state, SPLIT_SEPARATOR_STRING, separator)?;
        self.finish_string_split_state(site, state)
    }

    /// Completes a stateful split after every observable conversion has finished.
    fn finish_string_split_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let limit = numeric_value(pending.values[SPLIT_LIMIT])
            .expect("normalized split limit is numeric") as u32;
        self.materialize_string_split(
            site,
            pending.values[SPLIT_STRING],
            pending.values[SPLIT_SEPARATOR_STRING],
            limit,
        )
    }

    /// Handles the allocation-free decision path when all conversion operands are primitive.
    fn finish_primitive_string_split(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        separator: Value,
        limit: Value,
    ) -> Result<(), ExecutionError> {
        let string = self.string_split_to_string(receiver)?;
        let limit = if limit.as_immediate() == Some(Immediate::Undefined) {
            u32::MAX
        } else {
            self.string_split_to_uint32(limit)?
        };
        let (separator_string, string) = if separator.as_immediate() == Some(Immediate::Undefined) {
            (Value::from_immediate(Immediate::Undefined), string)
        } else {
            self.string_split_to_string_retaining(separator, string)?
        };
        self.materialize_string_split(site, string, separator_string, limit)
    }

    /// Builds the intrinsic result Array and publishes each UTF-16 substring in source order.
    fn materialize_string_split(
        &mut self,
        site: NativeContinuationSite,
        string: Value,
        separator: Value,
        limit: u32,
    ) -> Result<(), ExecutionError> {
        let source = self.primitive_string_units(string)?;
        let separator_units = if separator.as_immediate() == Some(Immediate::Undefined) {
            None
        } else {
            Some(self.primitive_string_units(separator)?)
        };
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before String.prototype.split");
        let result = self.create_array_object_with_prototype(prototype)?;
        self.write(site.caller_base, site.destination, result)?;
        if limit == 0 {
            return Ok(());
        }
        let Some(separator_units) = separator_units else {
            return self.string_split_push_units(site, 0, &source);
        };
        if source.is_empty() {
            if separator_units.is_empty() {
                return Ok(());
            }
            return self.string_split_push_units(site, 0, &source);
        }
        if separator_units.is_empty() {
            for (index, unit) in source.iter().take(limit as usize).enumerate() {
                self.string_split_push_units(site, index as u32, &[*unit])?;
            }
            return Ok(());
        }
        self.string_split_nonempty_separator(site, &source, &separator_units, limit)
    }

    /// Scans a nonempty separator without allocating an intermediate match-position vector.
    fn string_split_nonempty_separator(
        &mut self,
        site: NativeContinuationSite,
        source: &[u16],
        separator: &[u16],
        limit: u32,
    ) -> Result<(), ExecutionError> {
        let mut output_index = 0_u32;
        let mut segment_start = 0;
        let mut search = 0;
        while search + separator.len() <= source.len() {
            if source[search..search + separator.len()] == *separator {
                self.string_split_push_units(site, output_index, &source[segment_start..search])?;
                output_index += 1;
                if output_index == limit {
                    return Ok(());
                }
                search += separator.len();
                segment_start = search;
            } else {
                search += 1;
            }
        }
        self.string_split_push_units(site, output_index, &source[segment_start..])
    }

    /// Allocates one substring and creates its standard Array data property.
    pub(crate) fn string_split_push_units(
        &mut self,
        site: NativeContinuationSite,
        index: u32,
        units: &[u16],
    ) -> Result<(), ExecutionError> {
        let key = self.property_key_atom(safe_integer_value(u64::from(index)))?;
        let value = self.allocate_runtime_string(
            JsString::try_from_utf16(units).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let result = self.read(site.caller_base, site.destination)?;
        self.set_own_data_property(result, key, value)
    }

    /// Creates one numeric result property while Array length growth remains centralized.
    pub(crate) fn string_split_push_value(
        &mut self,
        result: Value,
        index: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.property_key_atom(safe_integer_value(u64::from(index)))?;
        self.set_own_data_property(result, key, value)
    }

    /// Converts an already primitive ECMAScript value to a managed String value.
    fn string_split_to_string(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_string_value(value) {
            return Ok(value);
        }
        let units = self.primitive_string_units(value)?;
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Converts one primitive to String while retaining an earlier managed String edge.
    fn string_split_to_string_retaining(
        &mut self,
        value: Value,
        retained: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        if self.is_string_value(value) {
            return Ok((value, retained));
        }
        self.primitive_string_value_retaining(Some(value), retained)
    }

    /// Applies the ECMAScript ToUint32 modulo rule to an already primitive value.
    fn string_split_to_uint32(&mut self, value: Value) -> Result<u32, ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        Ok(if !number.is_finite() || number == 0.0 {
            0
        } else {
            number.trunc().rem_euclid(UINT32_MODULUS) as u32
        })
    }

    /// Allocates the fixed five-value state shared by split conversion and callback stages.
    fn allocate_string_split_state(
        &mut self,
        receiver: Value,
        separator: Value,
        limit: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = StringSplitRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            pending: NativeCallState {
                values: [receiver, limit, separator, undefined, undefined],
                count: 2,
            },
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Updates one split state edge and records the generational write barrier.
    fn update_string_split_value(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}
