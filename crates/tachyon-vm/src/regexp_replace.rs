//! Resumable functional replacement for branded RegExp and primitive String searches.

use core::mem::size_of;

use tachyon_gc::{AllocationSpace, GcExternalMemory, GcRef, Trace, Tracer};
use tachyon_value::{Immediate, Value};

use crate::regexp::backend::{RegExpMatch, RegExpNamedCapture};
use crate::{
    CallSite, CompletionStackError, ConversionConsumer, ExecutionError, Isolate, JsString,
    NativeContinuation, NativeContinuationSite, VmRoots, safe_integer_value,
};

/// GC-owned callback arguments, match ranges, and output retained across JavaScript calls.
#[derive(Debug)]
pub(crate) struct PendingRegExpReplace {
    receiver: Value,
    input: Value,
    replacer: Value,
    temporary: Value,
    arguments: Box<[Value]>,
    input_units: Box<[u16]>,
    matches: Box<[RegExpMatch]>,
    output: Vec<u16>,
    match_index: usize,
    next_source_position: usize,
}

impl Trace for PendingRegExpReplace {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.input.trace(tracer);
        self.replacer.trace(tracer);
        self.temporary.trace(tracer);
        self.arguments.trace(tracer);
    }
}

#[derive(Clone)]
struct RegExpReplaceSnapshot {
    input: Value,
    replacer: Value,
    arguments: Box<[Value]>,
    matched: RegExpMatch,
}

impl Isolate {
    /// Starts one functional RegExp replacement after the branded fast path has compiled matches.
    pub(crate) fn begin_regexp_functional_replace(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        input: Value,
        replacer: Value,
        input_units: Vec<u16>,
        matches: Vec<RegExpMatch>,
    ) -> Result<(), ExecutionError> {
        if matches.is_empty() {
            return self.write(site.caller_base, site.destination, input);
        }
        let max_capture_count = matches
            .iter()
            .map(|matched| matched.captures.len())
            .max()
            .unwrap_or(0);
        let has_groups = matches
            .iter()
            .any(|matched| !matched.named_captures.is_empty());
        let argument_count = max_capture_count
            .checked_add(if has_groups { 4 } else { 3 })
            .ok_or(ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut output = Vec::new();
        output
            .try_reserve_exact(input_units.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let state = self.allocate_regexp_replace_state(PendingRegExpReplace {
            receiver,
            input,
            replacer,
            temporary: undefined,
            arguments: vec![undefined; argument_count].into_boxed_slice(),
            input_units: input_units.into_boxed_slice(),
            matches: matches.into_boxed_slice(),
            output,
            match_index: 0,
            next_source_position: 0,
        })?;
        self.root_regexp_replace_state(site, state)?;
        self.advance_regexp_functional_replace(site, state)
    }

    /// Starts primitive String functional replacement with its single exact match range.
    pub(crate) fn begin_string_functional_replace(
        &mut self,
        site: &CallSite,
        input: Value,
        replacer: Value,
        input_units: Vec<u16>,
        start: usize,
        end: usize,
    ) -> Result<(), ExecutionError> {
        self.begin_regexp_functional_replace(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            Value::from_immediate(Immediate::Undefined),
            input,
            replacer,
            input_units,
            vec![RegExpMatch {
                start,
                end,
                captures: Vec::new(),
                named_captures: Vec::new(),
            }],
        )
    }

    /// Resumes after the replacer callback and performs its observable ToString conversion.
    pub(crate) fn resume_regexp_replace_callback(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_regexp_replace_reference(continuation.first())?;
        self.root_regexp_replace_state(continuation.site(), state)?;
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::RegExpReplaceResult,
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_heap_ref(state.raw()),
                value,
                continuation.site().call_site,
            );
        }
        self.resume_regexp_replace_conversion(continuation.site(), state, value)
    }

    /// Appends an already primitive callback result and advances to the next global match.
    pub(crate) fn resume_regexp_replace_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingRegExpReplace>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        let mut replacement = Vec::new();
        replacement
            .try_reserve_exact(self.primitive_string_unit_length(value)?)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        self.append_primitive_string_units(value, &mut replacement)?;
        self.append_regexp_replace_result(state, &replacement)?;
        self.root_regexp_replace_state(site, state)?;
        self.advance_regexp_functional_replace(site, state)
    }

    /// Materializes one callback argument list, invokes it, and finalizes after the last result.
    fn advance_regexp_functional_replace(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingRegExpReplace>,
    ) -> Result<(), ExecutionError> {
        let Some(snapshot) = self.regexp_replace_snapshot(state)? else {
            let output = self.take_regexp_replace_output(state)?;
            let result = self.allocate_runtime_string(
                JsString::try_from_owned_code_units(output)
                    .map_err(ExecutionError::PropertyKeyString)?,
            )?;
            return self.write(site.caller_base, site.destination, result);
        };
        self.materialize_regexp_replace_arguments(state, &snapshot)?;
        let pending = self
            .regexp_replace_snapshot(state)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let argument_count = u32::try_from(pending.arguments.len())
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
        let prefix = self.create_apply_argument_prefix(
            pending.replacer,
            Value::from_immediate(Immediate::Undefined),
            pending.arguments.into_vec(),
        )?;
        let continuation = NativeContinuation::regexp_replace(
            site,
            Value::from_heap_ref(state.raw()),
            pending.replacer,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })?;
        let frame_depth = self.fiber.frames.len();
        let call_result = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: pending.replacer,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: argument_count,
            argument_count,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        });
        if let Err(error) = call_result {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a functional replacer publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_regexp_replace_callback(continuation, value)
    }

    /// Allocates match, captures, position, input, and optional named-groups arguments in order.
    fn materialize_regexp_replace_arguments(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
        snapshot: &RegExpReplaceSnapshot,
    ) -> Result<(), ExecutionError> {
        let matched = self.allocate_regexp_replace_state_substring(
            state,
            snapshot.matched.start..snapshot.matched.end,
        )?;
        self.set_regexp_replace_argument(state, 0, matched)?;
        for (index, range) in snapshot.matched.captures.iter().enumerate() {
            let value = match range {
                Some(range) => {
                    self.allocate_regexp_replace_state_substring(state, range.clone())?
                }
                None => Value::from_immediate(Immediate::Undefined),
            };
            self.set_regexp_replace_argument(state, index + 1, value)?;
        }
        let tail = snapshot.matched.captures.len() + 1;
        self.set_regexp_replace_argument(
            state,
            tail,
            safe_integer_value(
                u64::try_from(snapshot.matched.start)
                    .map_err(|_| ExecutionError::InvalidStringLength)?,
            ),
        )?;
        self.set_regexp_replace_argument(state, tail + 1, snapshot.input)?;
        if !snapshot.matched.named_captures.is_empty() {
            self.materialize_regexp_replace_groups(
                state,
                tail + 2,
                &snapshot.matched.named_captures,
            )?;
        }
        Ok(())
    }

    /// Builds the null-prototype groups object and roots it before defining capture properties.
    fn materialize_regexp_replace_groups(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
        argument_index: usize,
        captures: &[RegExpNamedCapture],
    ) -> Result<(), ExecutionError> {
        let groups =
            self.create_ordinary_object_with_prototype(Value::from_immediate(Immediate::Null))?;
        self.set_regexp_replace_argument(state, argument_index, groups)?;
        for capture in captures {
            let atom = self.intern_intrinsic_name(capture.name.as_bytes())?;
            let value = match &capture.range {
                Some(range) => {
                    self.allocate_regexp_replace_state_substring(state, range.clone())?
                }
                None => Value::from_immediate(Immediate::Undefined),
            };
            self.set_regexp_replace_temporary(state, value)?;
            self.set_own_data_property(groups, atom, value)?;
        }
        Ok(())
    }

    #[inline]
    fn allocate_regexp_replace_state_substring(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
        range: core::ops::Range<usize>,
    ) -> Result<Value, ExecutionError> {
        let string = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_regexp_replace)
                    .map_err(ExecutionError::NoGcBorrow)?;
                JsString::try_from_utf16(&pending.input_units[range])
                    .map_err(ExecutionError::PropertyKeyString)
            })
        })?;
        self.allocate_runtime_string(string)
    }

    /// Appends the untouched prefix and callback output, then advances scalar cursors atomically.
    fn append_regexp_replace_result(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
        replacement: &[u16],
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_regexp_replace)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let matched = pending
                    .matches
                    .get(pending.match_index)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                let additional = matched
                    .start
                    .saturating_sub(pending.next_source_position)
                    .saturating_add(replacement.len());
                pending
                    .output
                    .try_reserve_exact(additional)
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                pending.output.extend_from_slice(
                    &pending.input_units[pending.next_source_position..matched.start],
                );
                pending.output.extend_from_slice(replacement);
                pending.next_source_position = matched.end;
                pending.match_index += 1;
                Ok(())
            })
        })
    }

    fn regexp_replace_snapshot(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
    ) -> Result<Option<RegExpReplaceSnapshot>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_regexp_replace)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let Some(matched) = pending.matches.get(pending.match_index) else {
                    return Ok(None);
                };
                Ok(Some(RegExpReplaceSnapshot {
                    input: pending.input,
                    replacer: pending.replacer,
                    arguments: pending.arguments.clone(),
                    matched: matched.clone(),
                }))
            })
        })
    }

    fn take_regexp_replace_output(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
    ) -> Result<Vec<u16>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_regexp_replace)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending
                    .output
                    .try_reserve_exact(
                        pending
                            .input_units
                            .len()
                            .saturating_sub(pending.next_source_position),
                    )
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                pending
                    .output
                    .extend_from_slice(&pending.input_units[pending.next_source_position..]);
                Ok(core::mem::take(&mut pending.output))
            })
        })
    }

    fn allocate_regexp_replace_state(
        &mut self,
        pending: PendingRegExpReplace,
    ) -> Result<GcRef<PendingRegExpReplace>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_regexp_replace,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_regexp_replace_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingRegExpReplace>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_regexp_replace)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn root_regexp_replace_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingRegExpReplace>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    /// Stores one newly allocated callback argument with the required generational barrier.
    fn set_regexp_replace_argument(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
        index: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_regexp_replace)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let slot = pending
                    .arguments
                    .get_mut(index)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                *slot = value;
                Ok(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Roots a transient property value while defining it on the named-groups object.
    fn set_regexp_replace_temporary(
        &mut self,
        state: GcRef<PendingRegExpReplace>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_regexp_replace)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .temporary = value;
                Ok(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}

impl GcExternalMemory for PendingRegExpReplace {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        let match_bytes = self.matches.iter().fold(0_usize, |bytes, matched| {
            let captures = matched
                .captures
                .capacity()
                .saturating_mul(size_of::<Option<core::ops::Range<usize>>>());
            let names = matched
                .named_captures
                .iter()
                .fold(0_usize, |bytes, capture| {
                    bytes
                        .saturating_add(size_of::<RegExpNamedCapture>())
                        .saturating_add(capture.name.len())
                });
            bytes
                .saturating_add(size_of::<RegExpMatch>())
                .saturating_add(captures)
                .saturating_add(names)
        });
        self.arguments
            .len()
            .saturating_mul(size_of::<Value>())
            .saturating_add(self.input_units.len().saturating_mul(size_of::<u16>()))
            .saturating_add(self.output.capacity().saturating_mul(size_of::<u16>()))
            .saturating_add(match_bytes)
    }
}
