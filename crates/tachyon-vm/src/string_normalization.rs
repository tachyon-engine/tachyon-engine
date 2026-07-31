//! Resumable Unicode normalization and deterministic non-Intl string comparison.

use core::cmp::Ordering;

use unicode_normalization::UnicodeNormalization;

use super::*;

const UNICODE_RECEIVER: usize = 0;
const UNICODE_ARGUMENT: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum StringUnicodeOperation {
    Normalize,
    LocaleCompare,
}

impl StringUnicodeOperation {
    #[inline(always)]
    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Normalize),
            1 => Some(Self::LocaleCompare),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NormalizationForm {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

struct StringUnicodeRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for StringUnicodeRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts `normalize`, retaining the generic receiver and form before observable conversion.
    pub(crate) fn begin_string_normalize(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_string_unicode(StringUnicodeOperation::Normalize, site)
    }

    /// Starts non-Intl `localeCompare`, retaining `that` before converting the receiver.
    pub(crate) fn begin_string_locale_compare(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_string_unicode(StringUnicodeOperation::LocaleCompare, site)
    }

    /// Resumes either receiver or argument ToString without retaining a stale moving-GC handle.
    pub(crate) fn resume_string_unicode_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let slot = match consumer {
            ConversionConsumer::StringUnicodeReceiver => UNICODE_RECEIVER,
            ConversionConsumer::StringUnicodeArgument => UNICODE_ARGUMENT,
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        self.update_native_call_state_value(state, slot, primitive)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let primitive = self.native_call_state_snapshot(state)?.values[slot];
        let string = self.primitive_to_string_value(primitive)?;
        let rooted = self.read(site.caller_base, site.destination)?;
        let state = self.native_call_state_reference(rooted)?;
        match consumer {
            ConversionConsumer::StringUnicodeReceiver => {
                self.update_native_call_state_value(state, UNICODE_RECEIVER, string)?;
                self.begin_string_unicode_argument(site, state)
            }
            ConversionConsumer::StringUnicodeArgument => {
                self.update_native_call_state_value(state, UNICODE_ARGUMENT, string)?;
                self.finish_string_unicode(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Allocates the fixed traced state shared by both two-step String algorithms.
    fn begin_string_unicode(
        &mut self,
        operation: StringUnicodeOperation,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if is_nullish(site.this_value) {
            return Err(ExecutionError::NotObject(site.this_value));
        }
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let state = self.allocate_string_unicode_state(operation, site.this_value, argument)?;
        self.write(
            native_site.caller_base,
            native_site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.begin_string_unicode_receiver(native_site, state)
    }

    /// Converts the receiver first, as required before observing `form` or `that`.
    fn begin_string_unicode_receiver(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[UNICODE_RECEIVER];
        if self.is_object_value(receiver) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringUnicodeReceiver,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                receiver,
                site.call_site,
            );
        }
        self.resume_string_unicode_conversion(
            site,
            state,
            ConversionConsumer::StringUnicodeReceiver,
            receiver,
        )
    }

    /// Converts the second operand after receiver ToString, skipping undefined normalize form.
    fn begin_string_unicode_argument(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let operation = StringUnicodeOperation::from_u8(snapshot.count)
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let argument = snapshot.values[UNICODE_ARGUMENT];
        if operation == StringUnicodeOperation::Normalize
            && argument.as_immediate() == Some(Immediate::Undefined)
        {
            return self.finish_string_unicode(site, state);
        }
        if self.is_object_value(argument) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringUnicodeArgument,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                argument,
                site.call_site,
            );
        }
        self.resume_string_unicode_conversion(
            site,
            state,
            ConversionConsumer::StringUnicodeArgument,
            argument,
        )
    }

    /// Runs the allocation-only Unicode kernel after both observable conversions are complete.
    fn finish_string_unicode(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let operation = StringUnicodeOperation::from_u8(snapshot.count)
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let result = match operation {
            StringUnicodeOperation::Normalize => {
                self.finish_string_normalize(snapshot.values, site, state)?
            }
            StringUnicodeOperation::LocaleCompare => {
                self.finish_string_locale_compare(snapshot.values)?
            }
        };
        self.write(site.caller_base, site.destination, result)
    }

    /// Validates the exact form spelling and preserves lone surrogates through normalization.
    fn finish_string_normalize(
        &mut self,
        values: [Value; 5],
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<Value, ExecutionError> {
        let form = if values[UNICODE_ARGUMENT].as_immediate() == Some(Immediate::Undefined) {
            NormalizationForm::Nfc
        } else {
            normalization_form(&self.primitive_string_units(values[UNICODE_ARGUMENT])?)?
        };
        let input = self.primitive_string_units(values[UNICODE_RECEIVER])?;
        let output = normalize_utf16(&input, form)?;
        if output == input {
            return Ok(values[UNICODE_RECEIVER]);
        }
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Applies QuickJS-style NFC canonical equivalence and deterministic code-point ordering.
    fn finish_string_locale_compare(
        &mut self,
        values: [Value; 5],
    ) -> Result<Value, ExecutionError> {
        let left = normalize_utf16(
            &self.primitive_string_units(values[UNICODE_RECEIVER])?,
            NormalizationForm::Nfc,
        )?;
        let right = normalize_utf16(
            &self.primitive_string_units(values[UNICODE_ARGUMENT])?,
            NormalizationForm::Nfc,
        )?;
        let comparison = compare_utf16_code_points(&left, &right);
        Ok(Value::from_f64(match comparison {
            Ordering::Less => -1.0,
            Ordering::Equal => 0.0,
            Ordering::Greater => 1.0,
        }))
    }

    /// Allocates a two-edge state while the call frame and pending payload cover both operands.
    fn allocate_string_unicode_state(
        &mut self,
        operation: StringUnicodeOperation,
        receiver: Value,
        argument: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = StringUnicodeRoots {
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
                values: [receiver, argument, undefined, undefined, undefined],
                count: operation as u8,
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
}

/// Parses only the four normalization form names admitted by ECMAScript.
fn normalization_form(units: &[u16]) -> Result<NormalizationForm, ExecutionError> {
    match units {
        [0x4e, 0x46, 0x43] => Ok(NormalizationForm::Nfc),
        [0x4e, 0x46, 0x44] => Ok(NormalizationForm::Nfd),
        [0x4e, 0x46, 0x4b, 0x43] => Ok(NormalizationForm::Nfkc),
        [0x4e, 0x46, 0x4b, 0x44] => Ok(NormalizationForm::Nfkd),
        _ => Err(ExecutionError::InvalidNormalizationForm),
    }
}

/// Normalizes scalar runs independently and copies unpaired UTF-16 surrogates unchanged.
fn normalize_utf16(input: &[u16], form: NormalizationForm) -> Result<Vec<u16>, ExecutionError> {
    let initial_capacity = input
        .len()
        .saturating_mul(tuning::strings::NORMALIZATION_INITIAL_EXPANSION_FACTOR)
        .min(u32::MAX as usize);
    let mut output = Vec::new();
    output
        .try_reserve_exact(initial_capacity)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    let mut scalar_run = Vec::new();
    scalar_run
        .try_reserve_exact(input.len())
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    let mut index = 0;
    while let Some(&unit) = input.get(index) {
        if let Some((scalar, consumed)) = decode_utf16_scalar(input, index) {
            scalar_run.push(scalar);
            index += consumed;
        } else {
            append_normalized_run(&mut output, &scalar_run, form)?;
            scalar_run.clear();
            try_append_utf16(&mut output, &[unit])?;
            index += 1;
        }
    }
    append_normalized_run(&mut output, &scalar_run, form)?;
    Ok(output)
}

/// Decodes one valid scalar, returning None for either kind of unpaired surrogate.
#[inline(always)]
fn decode_utf16_scalar(input: &[u16], index: usize) -> Option<(char, usize)> {
    let first = *input.get(index)?;
    if (0xd800..=0xdbff).contains(&first) {
        let second = *input.get(index + 1)?;
        if !(0xdc00..=0xdfff).contains(&second) {
            return None;
        }
        let scalar = 0x1_0000 + (u32::from(first - 0xd800) << 10) + u32::from(second - 0xdc00);
        return char::from_u32(scalar).map(|character| (character, 2));
    }
    if (0xdc00..=0xdfff).contains(&first) {
        return None;
    }
    char::from_u32(u32::from(first)).map(|character| (character, 1))
}

/// Appends one valid scalar run through a fallible selected normalization iterator.
fn append_normalized_run(
    output: &mut Vec<u16>,
    run: &[char],
    form: NormalizationForm,
) -> Result<(), ExecutionError> {
    match form {
        NormalizationForm::Nfc => {
            for character in run.iter().copied().nfc() {
                try_append_normalized_char(output, character)?;
            }
        }
        NormalizationForm::Nfd => {
            for character in run.iter().copied().nfd() {
                try_append_normalized_char(output, character)?;
            }
        }
        NormalizationForm::Nfkc => {
            for character in run.iter().copied().nfkc() {
                try_append_normalized_char(output, character)?;
            }
        }
        NormalizationForm::Nfkd => {
            for character in run.iter().copied().nfkd() {
                try_append_normalized_char(output, character)?;
            }
        }
    }
    Ok(())
}

/// Encodes and appends one normalized scalar through the checked UTF-16 growth path.
#[inline]
fn try_append_normalized_char(
    output: &mut Vec<u16>,
    character: char,
) -> Result<(), ExecutionError> {
    let mut encoded = [0; 2];
    try_append_utf16(output, character.encode_utf16(&mut encoded))
}

/// Enforces ECMAScript's String length bound and uses only fallible Vec growth.
#[inline]
fn try_append_utf16(output: &mut Vec<u16>, units: &[u16]) -> Result<(), ExecutionError> {
    output
        .len()
        .checked_add(units.len())
        .filter(|length| *length <= u32::MAX as usize)
        .ok_or(ExecutionError::InvalidStringLength)?;
    output
        .try_reserve(units.len())
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    output.extend_from_slice(units);
    Ok(())
}

/// Compares normalized UTF-16 as scalar values while ordering lone surrogates by code unit.
fn compare_utf16_code_points(left: &[u16], right: &[u16]) -> Ordering {
    let mut left_index = 0;
    let mut right_index = 0;
    loop {
        let left_value = next_comparison_scalar(left, &mut left_index);
        let right_value = next_comparison_scalar(right, &mut right_index);
        match (left_value, right_value) {
            (Some(left_value), Some(right_value)) if left_value == right_value => {}
            (Some(left_value), Some(right_value)) => return left_value.cmp(&right_value),
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

/// Produces one valid scalar or one unpaired surrogate numeric value for comparison.
#[inline(always)]
fn next_comparison_scalar(input: &[u16], index: &mut usize) -> Option<u32> {
    let first = *input.get(*index)?;
    if let Some((scalar, consumed)) = decode_utf16_scalar(input, *index) {
        *index += consumed;
        Some(u32::from(scalar))
    } else {
        *index += 1;
        Some(u32::from(first))
    }
}
