//! Resumable RegExpExec dispatch shared by observable RegExp protocol methods.

use super::*;

const REGEXP_TEST_INPUT: usize = 0;
const REGEXP_TEST_RECEIVER: usize = 1;
pub(crate) const REGEXP_EXEC_RESULT: usize = 2;
pub(crate) const REGEXP_EXEC_GROUPS: usize = 3;
pub(crate) const REGEXP_EXEC_TEMPORARY: usize = 4;

struct RegExpTestRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for RegExpTestRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts branded `RegExp.prototype.exec`, requiring the internal slot before ToString.
    pub(crate) fn begin_regexp_exec(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        self.regexp_data(receiver)?;
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
                ConversionConsumer::RegExpExecInput,
                native_site.caller_base,
                native_site.destination,
                receiver,
                input,
                native_site.call_site,
            );
        }
        let input = self.regexp_string_argument(Some(input))?;
        self.finish_regexp_exec(native_site, receiver, input)
    }

    /// Continues branded exec after an object input has completed string-hint ToPrimitive.
    pub(crate) fn resume_regexp_exec_conversion(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let input = self.regexp_string_argument(Some(primitive))?;
        self.finish_regexp_exec(site, receiver, input)
    }

    /// Starts `RegExp.prototype.test`, preserving ToString before the observable `exec` lookup.
    pub(crate) fn begin_regexp_test(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
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
                ConversionConsumer::RegExpTestInput,
                native_site.caller_base,
                native_site.destination,
                receiver,
                input,
                native_site.call_site,
            );
        }
        let input = self.regexp_string_argument(Some(input))?;
        self.begin_regexp_test_exec(native_site, receiver, input)
    }

    /// Continues test after an object input has completed string-hint ToPrimitive.
    pub(crate) fn resume_regexp_test_conversion(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let input = self.regexp_string_argument(Some(primitive))?;
        self.begin_regexp_test_exec(site, receiver, input)
    }

    /// Resumes either the `exec` getter or the custom receiver-preserving call.
    pub(crate) fn resume_regexp_test(
        &mut self,
        continuation: NativeContinuation,
        stage: RegExpTestStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            RegExpTestStage::ExecGet => {
                if self.is_callable_value(value)? {
                    self.dispatch_property_callback(
                        NativeContinuation::regexp_test(
                            continuation.site(),
                            RegExpTestStage::ExecCall,
                            continuation.first(),
                            continuation.second(),
                        ),
                        value,
                    )?;
                    return Ok(());
                }
                self.finish_regexp_test_builtin(continuation)
            }
            RegExpTestStage::ExecCall => {
                if value.as_immediate() == Some(Immediate::Null) {
                    return self.write_regexp_test_boolean(continuation.site(), false);
                }
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.write_regexp_test_boolean(continuation.site(), true)
            }
            RegExpTestStage::LastIndexGet => {
                let state = self.native_call_state_reference(continuation.first())?;
                self.root_regexp_exec_state(continuation.site(), state)?;
                self.convert_regexp_last_index(continuation.site(), state, value)
            }
            RegExpTestStage::LastIndexSet => {
                let state = self.native_call_state_reference(continuation.first())?;
                self.root_regexp_exec_state(continuation.site(), state)?;
                self.finish_regexp_builtin_output(continuation.site(), state)
            }
        }
    }

    /// Looks up `exec` through ordinary accessors and Proxy [[Get]].
    fn begin_regexp_test_exec(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        input: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.allocate_regexp_exec_state(receiver, input, 1)?;
        let continuation = NativeContinuation::regexp_test(
            site,
            RegExpTestStage::ExecGet,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let exec = self.intern_intrinsic_name(b"exec")?;
        if let Err(error) =
            self.dispatch_proxy_aware_property_read(site, receiver, receiver, exec.into())
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
        let exec = self.read(site.caller_base, site.destination)?;
        self.resume_regexp_test(continuation, RegExpTestStage::ExecGet, exec)
    }

    /// Runs the branded builtin fallback while the fixed state remains rooted in a VM register.
    fn finish_regexp_test_builtin(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        self.write(site.caller_base, site.destination, continuation.first())?;
        let state = self.native_call_state_reference(continuation.first())?;
        self.begin_regexp_builtin(site, state)
    }

    /// Allocates the exact input/receiver state used by getter, call, and builtin branches.
    pub(crate) fn allocate_regexp_exec_state(
        &mut self,
        receiver: Value,
        input: Value,
        argument_count: u8,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = RegExpTestRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending: NativeCallState {
                values: [input, receiver, undefined, undefined, undefined],
                count: argument_count,
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

    /// Roots all exec result intermediates through one fixed state until publication completes.
    fn finish_regexp_exec(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        input: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.allocate_regexp_exec_state(receiver, input, 0)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.begin_regexp_builtin(site, state)
    }

    /// Starts the mandatory observable `Get(R, "lastIndex")` for RegExpBuiltinExec.
    fn begin_regexp_builtin(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let receiver = pending.values[REGEXP_TEST_RECEIVER];
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_regexp_exec_parent(site, state, RegExpTestStage::LastIndexGet, receiver)?;
        let last_index = self.intern_intrinsic_name(b"lastIndex")?;
        if let Err(error) =
            self.dispatch_proxy_aware_property_read(site, receiver, receiver, last_index.into())
        {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_regexp_test(continuation, RegExpTestStage::LastIndexGet, value)
    }

    /// Resumes object ToPrimitive work for one observed lastIndex value.
    pub(crate) fn resume_regexp_last_index_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_regexp_exec_state(site, state)?;
        self.finish_regexp_last_index(site, state, primitive)
    }

    /// Converts a primitive immediately or dispatches resumable number-hint ToPrimitive.
    fn convert_regexp_last_index(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::RegExpLastIndex,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_regexp_last_index(site, state, value)
    }

    /// Applies ToLength, executes the backend, and performs a required strict write-back.
    fn finish_regexp_last_index(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let last_index = regexp_to_length(self.convert_to_number(value)?)?;
        let pending = self.native_call_state_snapshot(state)?;
        let receiver = pending.values[REGEXP_TEST_RECEIVER];
        let input = pending.values[REGEXP_TEST_INPUT];
        let outcome = if pending.count == 0 {
            self.regexp_builtin_exec(receiver, input, state, last_index)?
        } else {
            self.regexp_builtin_test(receiver, input, last_index)?
        };
        self.update_regexp_exec_state_value(state, REGEXP_EXEC_TEMPORARY, outcome.value)?;
        let Some(last_index) = outcome.last_index else {
            return self.finish_regexp_builtin_output(site, state);
        };
        self.dispatch_regexp_last_index_set(site, state, receiver, last_index)
    }

    /// Performs `Set(R, "lastIndex", value, true)` and resumes the rooted builtin state.
    fn dispatch_regexp_last_index_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_regexp_exec_parent(site, state, RegExpTestStage::LastIndexSet, value)?;
        let last_index = self.intern_intrinsic_name(b"lastIndex")?;
        if let Err(error) = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            last_index.into(),
            value,
            ProxySetMode::ObjectAssign,
        ) {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        self.resume_regexp_test(continuation, RegExpTestStage::LastIndexSet, value)
    }

    /// Returns either the materialized exec result or the test-only boolean from fixed state.
    fn finish_regexp_builtin_output(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let result = self.native_call_state_snapshot(state)?.values[REGEXP_EXEC_TEMPORARY];
        self.write(site.caller_base, site.destination, result)
    }

    /// Roots RegExp state across one nested observable property operation.
    fn push_regexp_exec_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: RegExpTestStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::regexp_test(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    #[inline(always)]
    fn root_regexp_exec_state(
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

    /// Publishes one managed exec intermediate before any subsequent allocation can collect it.
    pub(crate) fn update_regexp_exec_state_value(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        debug_assert!(slot < 5);
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

    #[inline]
    fn write_regexp_test_boolean(
        &mut self,
        site: NativeContinuationSite,
        matched: bool,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(if matched {
                Immediate::True
            } else {
                Immediate::False
            }),
        )
    }
}

/// Applies ECMAScript ToLength to an already numeric primitive.
#[inline(always)]
fn regexp_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}
