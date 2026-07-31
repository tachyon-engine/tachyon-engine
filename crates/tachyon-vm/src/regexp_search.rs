//! Resumable String/RegExp search protocol integration.

use super::*;

const SEARCH_INPUT: usize = 0;
const SEARCH_RECEIVER: usize = 1;
const SEARCH_PREVIOUS_LAST_INDEX: usize = 2;
const SEARCH_RESULT: usize = 3;

impl Isolate {
    /// Starts RegExp.prototype[Symbol.search], converting input before observing lastIndex.
    pub(crate) fn begin_regexp_search(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        if !self.is_object_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let input = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(input) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::RegExpSearchInput,
                native_site.caller_base,
                native_site.destination,
                receiver,
                input,
                native_site.call_site,
            );
        }
        let input = self.regexp_string_argument(Some(input))?;
        self.begin_regexp_search_last_index(native_site, receiver, input)
    }

    /// Resumes RegExp search after an object input has completed string conversion.
    pub(crate) fn resume_regexp_search_input_conversion(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let input = self.regexp_string_argument(Some(primitive))?;
        self.begin_regexp_search_last_index(site, receiver, input)
    }

    /// Starts String.prototype.search with the @@search lookup before receiver conversion.
    pub(crate) fn begin_string_search(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        if is_nullish(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let pattern = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let state = self.allocate_regexp_exec_state(pattern, receiver, 1)?;
        self.root_regexp_search_state(native_site, state)?;
        if self.is_object_value(pattern) {
            self.begin_regexp_search_symbol_lookup(
                native_site,
                state,
                pattern,
                RegExpSearchStage::StringMethodGet,
            )
        } else {
            self.begin_string_search_receiver_conversion(native_site, state)
        }
    }

    /// Resumes String search ToPrimitive work for either receiver or pattern.
    pub(crate) fn resume_string_search_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_regexp_search_state(site, state)?;
        match consumer {
            ConversionConsumer::StringSearchReceiver => {
                let input = self.regexp_string_argument(Some(primitive))?;
                self.update_regexp_exec_state_value(state, SEARCH_INPUT, input)?;
                self.begin_string_search_pattern_conversion(site, state)
            }
            ConversionConsumer::StringSearchPattern => {
                self.finish_string_search_pattern(site, state, primitive)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Advances a completed observable Get, Set, or Call in either search protocol.
    pub(crate) fn resume_regexp_search(
        &mut self,
        continuation: NativeContinuation,
        stage: RegExpSearchStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        self.root_regexp_search_state(continuation.site(), state)?;
        match stage {
            RegExpSearchStage::StringMethodGet if is_nullish(value) => {
                self.begin_string_search_receiver_conversion(continuation.site(), state)
            }
            RegExpSearchStage::StringMethodGet => self.call_regexp_search_method(
                continuation.site(),
                state,
                value,
                RegExpSearchStage::StringMethodCall,
            ),
            RegExpSearchStage::StringMethodCall | RegExpSearchStage::StringCreatedMethodCall => {
                self.write(
                    continuation.site().caller_base,
                    continuation.site().destination,
                    value,
                )
            }
            RegExpSearchStage::StringCreatedMethodGet => self.call_regexp_search_method(
                continuation.site(),
                state,
                value,
                RegExpSearchStage::StringCreatedMethodCall,
            ),
            RegExpSearchStage::PreviousLastIndexGet => {
                self.update_regexp_exec_state_value(state, SEARCH_PREVIOUS_LAST_INDEX, value)?;
                if self.same_value(value, Value::from_i32(0))? {
                    self.begin_regexp_search_exec_lookup(continuation.site(), state)
                } else {
                    self.dispatch_regexp_search_last_index_set(
                        continuation.site(),
                        state,
                        Value::from_i32(0),
                        RegExpSearchStage::ZeroLastIndexSet,
                    )
                }
            }
            RegExpSearchStage::ZeroLastIndexSet => {
                self.begin_regexp_search_exec_lookup(continuation.site(), state)
            }
            RegExpSearchStage::ExecGet if self.is_callable_value(value)? => self
                .call_regexp_search_method(
                    continuation.site(),
                    state,
                    value,
                    RegExpSearchStage::ExecCall,
                ),
            RegExpSearchStage::ExecGet => {
                self.finish_regexp_search_builtin(continuation.site(), state)
            }
            RegExpSearchStage::ExecCall => {
                self.validate_regexp_exec_result(value)?;
                self.update_regexp_exec_state_value(state, SEARCH_RESULT, value)?;
                self.begin_regexp_search_current_last_index(continuation.site(), state)
            }
            RegExpSearchStage::BuiltinLastIndexSet => {
                self.begin_regexp_search_current_last_index(continuation.site(), state)
            }
            RegExpSearchStage::CurrentLastIndexGet => {
                let previous =
                    self.native_call_state_snapshot(state)?.values[SEARCH_PREVIOUS_LAST_INDEX];
                if self.same_value(value, previous)? {
                    self.finish_regexp_search_result(continuation.site(), state)
                } else {
                    self.dispatch_regexp_search_last_index_set(
                        continuation.site(),
                        state,
                        previous,
                        RegExpSearchStage::RestoreLastIndexSet,
                    )
                }
            }
            RegExpSearchStage::RestoreLastIndexSet => {
                self.finish_regexp_search_result(continuation.site(), state)
            }
            RegExpSearchStage::ResultIndexGet => self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                value,
            ),
        }
    }

    /// Converts the String receiver before converting or copying the RegExp pattern.
    fn begin_string_search_receiver_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[SEARCH_INPUT];
        if self.is_object_value(receiver) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringSearchReceiver,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                receiver,
                site.call_site,
            );
        }
        let input = self.regexp_string_argument(Some(receiver))?;
        self.update_regexp_exec_state_value(state, SEARCH_INPUT, input)?;
        self.begin_string_search_pattern_conversion(site, state)
    }

    /// Converts a non-RegExp object pattern only after the receiver conversion completed.
    fn begin_string_search_pattern_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pattern = self.native_call_state_snapshot(state)?.values[SEARCH_RECEIVER];
        if self.is_object_value(pattern) && self.regexp_data(pattern).is_err() {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringSearchPattern,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                pattern,
                site.call_site,
            );
        }
        self.finish_string_search_pattern(site, state, pattern)
    }

    /// Creates the fallback RegExp and performs the required observable @@search invocation.
    fn finish_string_search_pattern(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        pattern: Value,
    ) -> Result<(), ExecutionError> {
        self.update_regexp_exec_state_value(state, SEARCH_RECEIVER, pattern)?;
        self.root_regexp_search_state(site, state)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::regexp_search(
                site,
                RegExpSearchStage::StringCreatedMethodGet,
                Value::from_heap_ref(state.raw()),
                pattern,
            ))
            .map_err(Self::completion_stack_error)?;
        let created = self.create_regexp_for_string_search(site, pattern);
        let continuation = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(continuation.first())?;
        let regexp = created?;
        self.update_regexp_exec_state_value(state, SEARCH_RECEIVER, regexp)?;
        self.begin_regexp_search_symbol_lookup(
            site,
            state,
            regexp,
            RegExpSearchStage::StringCreatedMethodGet,
        )
    }

    /// Reads one symbol method through Proxy/accessor-aware [[Get]].
    fn begin_regexp_search_symbol_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        stage: RegExpSearchStage,
    ) -> Result<(), ExecutionError> {
        let symbol = self
            .agent
            .well_known_symbols
            .search
            .expect("Symbol.search initializes before String.prototype.search");
        let key = self.property_key(symbol)?;
        self.dispatch_regexp_search_read(site, state, receiver, key, stage)
    }

    /// Invokes one previously resolved search/exec method with the state input argument.
    fn call_regexp_search_method(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        method: Value,
        stage: RegExpSearchStage,
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(method)?;
        let receiver = self.native_call_state_snapshot(state)?.values[SEARCH_RECEIVER];
        self.dispatch_property_callback(
            NativeContinuation::regexp_search(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                receiver,
            ),
            method,
        )
        .map(|_| ())
    }

    /// Allocates the fixed search state and starts the first observable lastIndex Get.
    fn begin_regexp_search_last_index(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        input: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.allocate_regexp_exec_state(receiver, input, 1)?;
        self.root_regexp_search_state(site, state)?;
        let key = self.intern_intrinsic_name(b"lastIndex")?;
        self.dispatch_regexp_search_read(
            site,
            state,
            receiver,
            key.into(),
            RegExpSearchStage::PreviousLastIndexGet,
        )
    }

    /// Starts the RegExpExec custom-exec lookup after lastIndex has been normalized.
    fn begin_regexp_search_exec_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[SEARCH_RECEIVER];
        let key = self.intern_intrinsic_name(b"exec")?;
        self.dispatch_regexp_search_read(
            site,
            state,
            receiver,
            key.into(),
            RegExpSearchStage::ExecGet,
        )
    }

    /// Executes genuine RegExp fallback and publishes its result before observable write-back.
    fn finish_regexp_search_builtin(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let input = snapshot.values[SEARCH_INPUT];
        let receiver = snapshot.values[SEARCH_RECEIVER];
        self.regexp_data(receiver)?;
        let exec_state = self.allocate_regexp_exec_state(receiver, input, 0)?;
        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::regexp_search(
                site,
                RegExpSearchStage::ExecGet,
                Value::from_heap_ref(state.raw()),
                Value::from_heap_ref(exec_state.raw()),
            ))
            .map_err(Self::completion_stack_error)?;
        let outcome = self.regexp_builtin_exec(receiver, input, exec_state, 0);
        if self.fiber.completions.len() > depth {
            self.pop_native_continuation()?;
        }
        let outcome = outcome?;
        self.update_regexp_exec_state_value(state, SEARCH_RESULT, outcome.value)?;
        if let Some(last_index) = outcome.last_index {
            self.dispatch_regexp_search_last_index_set(
                site,
                state,
                last_index,
                RegExpSearchStage::BuiltinLastIndexSet,
            )
        } else {
            self.begin_regexp_search_current_last_index(site, state)
        }
    }

    /// Reads current lastIndex after RegExpExec so it can be restored with SameValue semantics.
    fn begin_regexp_search_current_last_index(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[SEARCH_RECEIVER];
        let key = self.intern_intrinsic_name(b"lastIndex")?;
        self.dispatch_regexp_search_read(
            site,
            state,
            receiver,
            key.into(),
            RegExpSearchStage::CurrentLastIndexGet,
        )
    }

    /// Returns -1 for null or performs the final observable result.index Get.
    fn finish_regexp_search_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let result = self.native_call_state_snapshot(state)?.values[SEARCH_RESULT];
        if result.as_immediate() == Some(Immediate::Null) {
            return self.write(site.caller_base, site.destination, Value::from_i32(-1));
        }
        let key = self.intern_intrinsic_name(b"index")?;
        self.dispatch_regexp_search_read(
            site,
            state,
            result,
            key.into(),
            RegExpSearchStage::ResultIndexGet,
        )
    }

    /// Performs a strict observable lastIndex write and resumes at the requested stage.
    fn dispatch_regexp_search_last_index_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
        stage: RegExpSearchStage,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[SEARCH_RECEIVER];
        let continuation = NativeContinuation::regexp_search(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let key = self.intern_intrinsic_name(b"lastIndex")?;
        if let Err(error) = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            key.into(),
            value,
            ProxySetMode::ObjectAssign,
        ) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        self.resume_regexp_search(continuation, stage, value)
    }

    /// Performs one observable property read while retaining the complete search state.
    fn dispatch_regexp_search_read(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        key: PropertyKey,
        stage: RegExpSearchStage,
    ) -> Result<(), ExecutionError> {
        let continuation = NativeContinuation::regexp_search(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_regexp_search(continuation, stage, value)
    }

    #[inline(always)]
    fn root_regexp_search_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    #[inline(always)]
    fn validate_regexp_exec_result(&self, result: Value) -> Result<(), ExecutionError> {
        if result.as_immediate() == Some(Immediate::Null) || self.is_object_value(result) {
            Ok(())
        } else {
            Err(ExecutionError::NotObject(result))
        }
    }
}
