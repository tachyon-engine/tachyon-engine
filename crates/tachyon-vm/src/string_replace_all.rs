//! Resumable `String.prototype.replaceAll` protocol and string replacement kernels.

use super::*;
use crate::{builtins::append_regexp_replacement, regexp::backend::RegExpMatch};

const REPLACE_ALL_RECEIVER: usize = 0;
const REPLACE_ALL_REPLACEMENT: usize = 1;
const REPLACE_ALL_SEARCH: usize = 2;
const REPLACE_ALL_INPUT: usize = 3;
const REPLACE_ALL_SEARCH_STRING: usize = 4;

struct StringReplaceAllRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for StringReplaceAllRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts replaceAll while preserving IsRegExp and GetMethod observable order.
    pub(crate) fn begin_string_replace_all(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        if is_nullish(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let replacement = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let state = self.allocate_string_replace_all_state(receiver, search, replacement)?;
        self.root_string_replace_all_state(native_site, state)?;
        if self.is_object_value(search) {
            self.begin_string_replace_all_match_lookup(native_site, state)
        } else {
            self.begin_string_replace_all_receiver_conversion(native_site, state)
        }
    }

    /// Resumes one Symbol.match, flags, Symbol.replace, or replacer-call boundary.
    pub(crate) fn resume_string_replace_all(
        &mut self,
        continuation: NativeContinuation,
        stage: StringReplaceAllStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.native_call_state_reference(continuation.first())?;
        self.root_string_replace_all_state(site, state)?;
        match stage {
            StringReplaceAllStage::MatchGet => {
                let search = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_SEARCH];
                let is_regexp = if value.as_immediate() == Some(Immediate::Undefined) {
                    self.is_regexp_value(search)
                } else {
                    self.is_truthy_value(value)?
                };
                if is_regexp {
                    self.begin_string_replace_all_flags_lookup(site, state)
                } else {
                    self.begin_string_replace_all_replace_lookup(site, state)
                }
            }
            StringReplaceAllStage::FlagsGet => {
                self.begin_string_replace_all_flags_conversion(site, state, value)
            }
            StringReplaceAllStage::ReplaceGet if is_nullish(value) => {
                self.begin_string_replace_all_receiver_conversion(site, state)
            }
            StringReplaceAllStage::ReplaceGet => {
                self.dispatch_string_replace_all_method(site, state, value)
            }
            StringReplaceAllStage::ReplaceCall => {
                self.write(site.caller_base, site.destination, value)
            }
        }
    }

    /// Continues one object ToPrimitive owned by the replaceAll state machine.
    pub(crate) fn resume_string_replace_all_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_string_replace_all_state(site, state)?;
        match consumer {
            ConversionConsumer::StringReplaceAllFlags => {
                self.finish_string_replace_all_flags(site, state, primitive)
            }
            ConversionConsumer::StringReplaceAllReceiver => {
                let input = self.string_replace_all_to_string(primitive)?;
                self.update_regexp_exec_state_value(state, REPLACE_ALL_INPUT, input)?;
                self.begin_string_replace_all_search_conversion(site, state)
            }
            ConversionConsumer::StringReplaceAllSearch => {
                let search = self.string_replace_all_to_string(primitive)?;
                self.update_regexp_exec_state_value(state, REPLACE_ALL_SEARCH_STRING, search)?;
                self.begin_string_replace_all_replacement_conversion(site, state)
            }
            ConversionConsumer::StringReplaceAllReplacement => {
                let replacement = self.string_replace_all_to_string(primitive)?;
                self.update_regexp_exec_state_value(state, REPLACE_ALL_REPLACEMENT, replacement)?;
                self.finish_string_replace_all_state(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Reads searchValue[Symbol.match] for the IsRegExp override.
    fn begin_string_replace_all_match_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let search = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_SEARCH];
        let symbol = self
            .agent
            .well_known_symbols
            .r#match
            .expect("Symbol.match initializes before replaceAll");
        let key = self.property_key(symbol)?;
        self.dispatch_string_replace_all_read(
            site,
            state,
            search,
            key,
            StringReplaceAllStage::MatchGet,
        )
    }

    /// Reads flags only after IsRegExp returned true.
    fn begin_string_replace_all_flags_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let search = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_SEARCH];
        let flags = self.intern_intrinsic_name(b"flags")?;
        if let Some(flags_value) = self.intrinsic_regexp_flags_value(search, flags)? {
            return self.finish_string_replace_all_flags(site, state, flags_value);
        }
        self.dispatch_string_replace_all_read(
            site,
            state,
            search,
            flags.into(),
            StringReplaceAllStage::FlagsGet,
        )
    }

    /// Uses the private flags slot only when the complete intrinsic accessor chain is unchanged.
    pub(crate) fn intrinsic_regexp_flags_value(
        &mut self,
        search: Value,
        flags: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        if !self.is_regexp_value(search)
            || self
                .complete_own_property_descriptor(search, flags)?
                .is_some()
        {
            return Ok(None);
        }
        let prototype = self.object_prototype_of(search)?;
        if self.realm.regexp_prototype != Some(prototype) {
            return Ok(None);
        }
        let Some(PropertyDescriptor::Accessor(descriptor)) =
            self.complete_own_property_descriptor(prototype, flags)?
        else {
            return Ok(None);
        };
        let Some(getter) = descriptor.getter else {
            return Ok(None);
        };
        if !matches!(
            self.resolve_function_executable(getter)?,
            FunctionExecutable::Native(NativeFunction::RegExpGetter(RegExpGetter::Flags))
        ) {
            return Ok(None);
        }
        self.regexp_data(search).map(|(_, flags)| Some(flags))
    }

    /// Converts flags with String hint before enforcing the mandatory global flag.
    fn begin_string_replace_all_flags_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        flags: Value,
    ) -> Result<(), ExecutionError> {
        if is_nullish(flags) {
            return Err(ExecutionError::RegExpMatchAllRequiresGlobal);
        }
        if self.is_object_value(flags) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringReplaceAllFlags,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                flags,
                site.call_site,
            );
        }
        self.finish_string_replace_all_flags(site, state, flags)
    }

    /// Validates that the converted flags String contains the `g` code unit.
    fn finish_string_replace_all_flags(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        flags: Value,
    ) -> Result<(), ExecutionError> {
        let flags = self.string_replace_all_to_string(flags)?;
        self.root_string_replace_all_state(site, state)?;
        if !self.regexp_string_units(flags)?.contains(&u16::from(b'g')) {
            return Err(ExecutionError::NotObject(flags));
        }
        self.begin_string_replace_all_replace_lookup(site, state)
    }

    /// Reads searchValue[Symbol.replace] after global validation.
    fn begin_string_replace_all_replace_lookup(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let search = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_SEARCH];
        let symbol = self
            .agent
            .well_known_symbols
            .replace
            .expect("Symbol.replace initializes before replaceAll");
        let key = self.property_key(symbol)?;
        self.dispatch_string_replace_all_read(
            site,
            state,
            search,
            key,
            StringReplaceAllStage::ReplaceGet,
        )
    }

    /// Calls a custom @@replace method with `(receiver, replaceValue)`.
    fn dispatch_string_replace_all_method(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        method: Value,
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(method)?;
        let search = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_SEARCH];
        self.dispatch_property_callback(
            NativeContinuation::string_replace_all(
                site,
                StringReplaceAllStage::ReplaceCall,
                Value::from_heap_ref(state.raw()),
                search,
            ),
            method,
        )
        .map(|_| ())
    }

    /// Converts the receiver before searchValue and replaceValue.
    fn begin_string_replace_all_receiver_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_RECEIVER];
        if self.is_object_value(receiver) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringReplaceAllReceiver,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                receiver,
                site.call_site,
            );
        }
        let input = self.string_replace_all_to_string(receiver)?;
        self.update_regexp_exec_state_value(state, REPLACE_ALL_INPUT, input)?;
        self.begin_string_replace_all_search_conversion(site, state)
    }

    /// Converts searchValue after receiver conversion and protocol delegation.
    fn begin_string_replace_all_search_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let search = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_SEARCH];
        if self.is_object_value(search) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringReplaceAllSearch,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                search,
                site.call_site,
            );
        }
        let search = self.string_replace_all_to_string(search)?;
        self.update_regexp_exec_state_value(state, REPLACE_ALL_SEARCH_STRING, search)?;
        self.begin_string_replace_all_replacement_conversion(site, state)
    }

    /// Converts a non-callable replacement only after both String operands.
    fn begin_string_replace_all_replacement_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let replacement = self.native_call_state_snapshot(state)?.values[REPLACE_ALL_REPLACEMENT];
        if self.is_callable_value(replacement)? {
            return self.finish_string_replace_all_state(site, state);
        }
        if self.is_object_value(replacement) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringReplaceAllReplacement,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                replacement,
                site.call_site,
            );
        }
        let replacement = self.string_replace_all_to_string(replacement)?;
        self.update_regexp_exec_state_value(state, REPLACE_ALL_REPLACEMENT, replacement)?;
        self.finish_string_replace_all_state(site, state)
    }

    /// Chooses the functional callback pipeline or the non-observable substitution kernel.
    fn finish_string_replace_all_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let input = pending.values[REPLACE_ALL_INPUT];
        let search = pending.values[REPLACE_ALL_SEARCH_STRING];
        let replacement = pending.values[REPLACE_ALL_REPLACEMENT];
        let input_units = self.regexp_string_units(input)?;
        let search_units = self.regexp_string_units(search)?;
        if self.is_callable_value(replacement)? {
            let matches = string_replace_all_matches(&input_units, &search_units)?;
            return self.begin_regexp_functional_replace(
                site,
                Value::from_immediate(Immediate::Undefined),
                input,
                replacement,
                input_units,
                matches,
            );
        }
        let replacement_units = self.regexp_string_units(replacement)?;
        let output =
            string_replace_all_substitution(&input_units, &search_units, &replacement_units)?;
        let output = self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )?;
        self.write(site.caller_base, site.destination, output)
    }

    /// Performs one Proxy/accessor-aware property read while retaining all operands.
    fn dispatch_string_replace_all_read(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        key: PropertyKey,
        stage: StringReplaceAllStage,
    ) -> Result<(), ExecutionError> {
        let continuation = NativeContinuation::string_replace_all(
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
        self.resume_string_replace_all(continuation, stage, value)
    }

    /// Converts an already primitive ToString input while rejecting Symbol.
    fn string_replace_all_to_string(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_value(Some(value))
    }

    /// Allocates the fixed state used by all protocol and conversion stages.
    fn allocate_string_replace_all_state(
        &mut self,
        receiver: Value,
        search: Value,
        replacement: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = StringReplaceAllRoots {
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
                values: [receiver, replacement, search, undefined, undefined],
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

    #[inline(always)]
    fn root_string_replace_all_state(
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
}

/// Precomputes non-overlapping match ranges for the existing functional replacement pipeline.
fn string_replace_all_matches(
    input: &[u16],
    search: &[u16],
) -> Result<Vec<RegExpMatch>, ExecutionError> {
    let estimate = if search.is_empty() {
        input.len().saturating_add(1)
    } else {
        input.len().checked_div(search.len()).unwrap_or(0)
    };
    let mut matches = Vec::new();
    matches
        .try_reserve_exact(estimate)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    for position in string_replace_all_positions(input, search) {
        matches.push(RegExpMatch {
            start: position,
            end: position + search.len(),
            captures: Vec::new(),
            named_captures: Vec::new(),
        });
    }
    Ok(matches)
}

/// Expands GetSubstitution for every non-overlapping String match.
fn string_replace_all_substitution(
    input: &[u16],
    search: &[u16],
    replacement: &[u16],
) -> Result<Vec<u16>, ExecutionError> {
    let mut output = Vec::new();
    let initial_capacity = input
        .len()
        .checked_add(replacement.len())
        .ok_or(ExecutionError::InvalidStringLength)?;
    output
        .try_reserve_exact(initial_capacity)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    let mut cursor = 0;
    for position in string_replace_all_positions(input, search) {
        try_append_string_replace_all_units(&mut output, &input[cursor..position])?;
        append_regexp_replacement(
            &mut output,
            replacement,
            input,
            &RegExpMatch {
                start: position,
                end: position + search.len(),
                captures: Vec::new(),
                named_captures: Vec::new(),
            },
        )?;
        cursor = position + search.len();
    }
    try_append_string_replace_all_units(&mut output, &input[cursor..])?;
    Ok(output)
}

/// Appends a checked static-replacement slice without using infallible `Vec` growth.
#[inline]
fn try_append_string_replace_all_units(
    output: &mut Vec<u16>,
    units: &[u16],
) -> Result<(), ExecutionError> {
    output
        .len()
        .checked_add(units.len())
        .ok_or(ExecutionError::InvalidStringLength)?;
    output
        .try_reserve(units.len())
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    output.extend_from_slice(units);
    Ok(())
}

/// Returns every non-overlapping UTF-16 match position, including empty-string boundaries.
fn string_replace_all_positions<'a>(
    input: &'a [u16],
    search: &'a [u16],
) -> impl Iterator<Item = usize> + 'a {
    let advance = search.len().max(1);
    let mut next = Some(0usize);
    core::iter::from_fn(move || {
        let from = next?;
        let relative = if search.is_empty() {
            (from <= input.len()).then_some(0)
        } else {
            input
                .get(from..)?
                .windows(search.len())
                .position(|window| window == search)
        }?;
        let position = from + relative;
        next = position
            .checked_add(advance)
            .filter(|next| *next <= input.len());
        Some(position)
    })
}
