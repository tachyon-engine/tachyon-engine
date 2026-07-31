//! Resumable observable protocols for `String.prototype.match` and `matchAll`.

use super::*;

const MATCH_RECEIVER: usize = 0;
const MATCH_PATTERN: usize = 1;
const MATCH_INPUT: usize = 2;
const MATCH_METHOD: usize = 3;
const MATCH_OPERATION: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum StringMatchOperation {
    Match,
    MatchAll,
}

impl StringMatchOperation {
    #[inline(always)]
    const fn from_value(value: Value) -> Option<Self> {
        match value.as_i32() {
            Some(0) => Some(Self::Match),
            Some(1) => Some(Self::MatchAll),
            _ => None,
        }
    }

    #[inline(always)]
    const fn value(self) -> Value {
        Value::from_i32(self as i32)
    }
}

struct StringMatchRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for StringMatchRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts `String.prototype.match` before any observable pattern operation.
    pub(crate) fn begin_string_match(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_string_match_protocol(site, StringMatchOperation::Match)
    }

    /// Starts `String.prototype.matchAll`, including the IsRegExp/global check.
    pub(crate) fn begin_string_match_all(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_string_match_protocol(site, StringMatchOperation::MatchAll)
    }

    /// Resumes a symbol/flags read or a custom/intrinsic symbol-method call.
    pub(crate) fn resume_string_match(
        &mut self,
        continuation: NativeContinuation,
        stage: StringMatchStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.native_call_state_reference(continuation.first())?;
        if matches!(
            stage,
            StringMatchStage::MethodGet | StringMatchStage::CreatedMethodGet
        ) && !is_nullish(value)
        {
            self.update_regexp_exec_state_value(state, MATCH_METHOD, value)?;
        }
        self.root_string_match_state(site, state)?;
        match stage {
            StringMatchStage::IsRegExpMatchGet => {
                let pattern = self.native_call_state_snapshot(state)?.values[MATCH_PATTERN];
                let is_regexp = if value.as_immediate() == Some(Immediate::Undefined) {
                    self.is_regexp_value(pattern)
                } else {
                    self.is_truthy_value(value)?
                };
                if is_regexp {
                    self.begin_string_match_flags_get(site, state)
                } else {
                    self.begin_string_match_method_get(site, state, false)
                }
            }
            StringMatchStage::FlagsGet => {
                self.begin_string_match_flags_conversion(site, state, value)
            }
            StringMatchStage::MethodGet if is_nullish(value) => {
                self.begin_string_match_receiver_conversion(site, state)
            }
            StringMatchStage::MethodGet => {
                self.call_string_match_method(site, state, value, StringMatchStage::MethodCall)
            }
            StringMatchStage::MethodCall | StringMatchStage::CreatedMethodCall => {
                self.write(site.caller_base, site.destination, value)
            }
            StringMatchStage::CreatedMethodGet => self.call_string_match_method(
                site,
                state,
                value,
                StringMatchStage::CreatedMethodCall,
            ),
        }
    }

    /// Continues receiver, fallback-pattern, or matchAll flags ToString conversion.
    pub(crate) fn resume_string_match_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_string_match_state(site, state)?;
        match consumer {
            ConversionConsumer::StringMatchReceiver => {
                let input = self.string_match_to_string(primitive)?;
                let state = self.reload_string_match_state(site)?;
                self.update_regexp_exec_state_value(state, MATCH_INPUT, input)?;
                self.begin_string_match_pattern_conversion(site, state)
            }
            ConversionConsumer::StringMatchPattern => {
                let pattern = self.string_match_to_string(primitive)?;
                let state = self.reload_string_match_state(site)?;
                self.finish_string_match_fallback(site, state, pattern)
            }
            ConversionConsumer::StringMatchAllFlags => {
                self.finish_string_match_flags(site, state, primitive)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Creates the fixed traced state before the first getter or callback can run.
    fn begin_string_match_protocol(
        &mut self,
        site: &CallSite,
        operation: StringMatchOperation,
    ) -> Result<(), ExecutionError> {
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
        let state = self.allocate_string_match_state(receiver, pattern, operation)?;
        self.root_string_match_state(native_site, state)?;
        if !self.is_object_value(pattern) {
            return self.begin_string_match_receiver_conversion(native_site, state);
        }
        if operation == StringMatchOperation::MatchAll {
            self.begin_string_match_is_regexp(native_site, state)
        } else {
            self.begin_string_match_method_get(native_site, state, false)
        }
    }

    /// Reads `pattern[@@match]` for the matchAll IsRegExp abstract operation.
    fn begin_string_match_is_regexp(
        &mut self,
        site: NativeContinuationSite,
        _state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let symbol = self
            .agent
            .well_known_symbols
            .r#match
            .expect("Symbol.match initializes before matchAll");
        let key = self.property_key(symbol)?;
        let state = self.reload_string_match_state(site)?;
        let pattern = self.native_call_state_snapshot(state)?.values[MATCH_PATTERN];
        self.dispatch_string_match_read(
            site,
            state,
            pattern,
            key,
            StringMatchStage::IsRegExpMatchGet,
        )
    }

    /// Reads flags only after IsRegExp returned true.
    fn begin_string_match_flags_get(
        &mut self,
        site: NativeContinuationSite,
        _state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let key = self.intern_intrinsic_name(b"flags")?;
        let state = self.reload_string_match_state(site)?;
        let pattern = self.native_call_state_snapshot(state)?.values[MATCH_PATTERN];
        if let Some(flags) = self.intrinsic_regexp_flags_value(pattern, key)? {
            let state = self.reload_string_match_state(site)?;
            return self.begin_string_match_flags_conversion(site, state, flags);
        }
        self.dispatch_string_match_read(
            site,
            state,
            pattern,
            key.into(),
            StringMatchStage::FlagsGet,
        )
    }

    /// Applies RequireObjectCoercible and resumable ToString to observed flags.
    fn begin_string_match_flags_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        flags: Value,
    ) -> Result<(), ExecutionError> {
        if is_nullish(flags) {
            return Err(ExecutionError::NotObject(flags));
        }
        if self.is_object_value(flags) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringMatchAllFlags,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                flags,
                site.call_site,
            );
        }
        self.finish_string_match_flags(site, state, flags)
    }

    /// Enforces the global flag before observing `pattern[@@matchAll]`.
    fn finish_string_match_flags(
        &mut self,
        site: NativeContinuationSite,
        _state: GcRef<NativeCallState>,
        flags: Value,
    ) -> Result<(), ExecutionError> {
        let flags = self.string_match_to_string(flags)?;
        let state = self.reload_string_match_state(site)?;
        if !self.regexp_string_units(flags)?.contains(&u16::from(b'g')) {
            return Err(ExecutionError::RegExpMatchAllRequiresGlobal);
        }
        self.begin_string_match_method_get(site, state, false)
    }

    /// Reads either the original or newly-created receiver's symbol method.
    fn begin_string_match_method_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        created: bool,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let operation = self.string_match_operation(&pending)?;
        let symbol = match operation {
            StringMatchOperation::Match => self.agent.well_known_symbols.r#match,
            StringMatchOperation::MatchAll => self.agent.well_known_symbols.match_all,
        }
        .expect("String match symbols initialize before String methods");
        let key = self.property_key(symbol)?;
        let state = self.reload_string_match_state(site)?;
        let receiver = self.native_call_state_snapshot(state)?.values[MATCH_PATTERN];
        let stage = if created {
            StringMatchStage::CreatedMethodGet
        } else {
            StringMatchStage::MethodGet
        };
        self.dispatch_string_match_read(site, state, receiver, key, stage)
    }

    /// Calls one resolved symbol method with the unconverted or converted receiver argument.
    fn call_string_match_method(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        method: Value,
        stage: StringMatchStage,
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(method)?;
        let receiver = self.native_call_state_snapshot(state)?.values[MATCH_PATTERN];
        self.dispatch_property_callback(
            NativeContinuation::string_match(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                receiver,
            ),
            method,
        )
        .map(|_| ())
    }

    /// Converts the String receiver only after protocol delegation has declined.
    fn begin_string_match_receiver_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[MATCH_RECEIVER];
        if self.is_object_value(receiver) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringMatchReceiver,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                receiver,
                site.call_site,
            );
        }
        let input = self.string_match_to_string(receiver)?;
        let state = self.reload_string_match_state(site)?;
        self.update_regexp_exec_state_value(state, MATCH_INPUT, input)?;
        self.begin_string_match_pattern_conversion(site, state)
    }

    /// Converts a non-RegExp object pattern for the RegExpCreate fallback.
    fn begin_string_match_pattern_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pattern = self.native_call_state_snapshot(state)?.values[MATCH_PATTERN];
        if self.is_object_value(pattern) && !self.is_regexp_value(pattern) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringMatchPattern,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                pattern,
                site.call_site,
            );
        }
        self.finish_string_match_fallback(site, state, pattern)
    }

    /// Performs RegExpCreate and then Invoke on the resulting intrinsic object.
    fn finish_string_match_fallback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        pattern: Value,
    ) -> Result<(), ExecutionError> {
        self.update_regexp_exec_state_value(state, MATCH_PATTERN, pattern)?;
        self.root_string_match_state(site, state)?;
        let pending = self.native_call_state_snapshot(state)?;
        let operation = self.string_match_operation(&pending)?;
        let continuation = NativeContinuation::string_match(
            site,
            StringMatchStage::CreatedMethodGet,
            Value::from_heap_ref(state.raw()),
            pattern,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let created = match operation {
            StringMatchOperation::Match => self.create_regexp_for_string_search(site, pattern),
            StringMatchOperation::MatchAll => {
                self.create_global_regexp_for_match_all(site, pattern)
            }
        };
        let continuation = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(continuation.first())?;
        let regexp = created?;
        self.update_regexp_exec_state_value(state, MATCH_PATTERN, regexp)?;
        let input = self.native_call_state_snapshot(state)?.values[MATCH_INPUT];
        self.update_regexp_exec_state_value(state, MATCH_RECEIVER, input)?;
        self.root_string_match_state(site, state)?;
        self.begin_string_match_method_get(site, state, true)
    }

    /// Performs one Proxy/accessor-aware property read with the full protocol rooted.
    fn dispatch_string_match_read(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        key: PropertyKey,
        stage: StringMatchStage,
    ) -> Result<(), ExecutionError> {
        let continuation = NativeContinuation::string_match(
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
        self.resume_string_match(continuation, stage, value)
    }

    /// Allocates exact fixed state, including the callback argument in slot zero.
    fn allocate_string_match_state(
        &mut self,
        receiver: Value,
        pattern: Value,
        operation: StringMatchOperation,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = StringMatchRoots {
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
                values: [receiver, pattern, undefined, undefined, operation.value()],
                count: 1,
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

    #[inline(always)]
    fn string_match_operation(
        &self,
        state: &NativeCallState,
    ) -> Result<StringMatchOperation, ExecutionError> {
        StringMatchOperation::from_value(state.values[MATCH_OPERATION])
            .ok_or(ExecutionError::MissingNativeContinuation)
    }

    #[inline(always)]
    fn string_match_to_string(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_value(Some(value))
    }

    #[inline(always)]
    fn root_string_match_state(
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

    /// Reloads the movable protocol state from its destination-register root after a safepoint.
    #[inline(always)]
    fn reload_string_match_state(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let rooted = self.read(site.caller_base, site.destination)?;
        self.native_call_state_reference(rooted)
    }
}
