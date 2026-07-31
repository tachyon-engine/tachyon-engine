//! Shared resumable conversion driver for generic String prototype algorithms.

use super::*;

const STRING_RECEIVER: usize = 0;
const STRING_FIRST: usize = 1;
const STRING_SECOND: usize = 2;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

struct StringPrototypeRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for StringPrototypeRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts one generic String method, retaining its operands before observable conversion.
    pub(crate) fn begin_string_prototype_operation(
        &mut self,
        operation: StringPrototypeOperation,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if is_nullish(site.this_value) {
            return Err(ExecutionError::NotObject(site.this_value));
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let first = self.call_argument(site, 0)?.unwrap_or(undefined);
        let second = self.call_argument(site, 1)?.unwrap_or(undefined);
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.string_operation_is_primitive_fast(operation, site.this_value, first, second) {
            return self.finish_fast_string_operation(
                native_site,
                operation,
                site.this_value,
                first,
                second,
            );
        }
        let state =
            self.allocate_string_prototype_state(operation, site.this_value, first, second)?;
        self.write(
            native_site.caller_base,
            native_site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.begin_string_prototype_receiver(native_site, state)
    }

    /// Continues a String-owned ToPrimitive after the callback returned one primitive value.
    pub(crate) fn resume_string_prototype_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::StringPrototypeReceiver => {
                let string = self.primitive_to_string_value(primitive)?;
                self.update_native_call_state_value(state, STRING_RECEIVER, string)?;
                self.begin_string_prototype_first(site, state)
            }
            ConversionConsumer::StringPrototypeString => {
                let string = self.primitive_to_string_value(primitive)?;
                self.update_native_call_state_value(state, STRING_FIRST, string)?;
                self.begin_string_prototype_after_first_string(site, state)
            }
            ConversionConsumer::StringPrototypeFiller => {
                let string = self.primitive_to_string_value(primitive)?;
                self.update_native_call_state_value(state, STRING_SECOND, string)?;
                self.finish_string_prototype_state(site, state)
            }
            ConversionConsumer::StringPrototypeFirstNumber => {
                let number = self.string_prototype_number(primitive)?;
                self.update_native_call_state_value(state, STRING_FIRST, number)?;
                self.begin_string_prototype_after_first_number(site, state)
            }
            ConversionConsumer::StringPrototypeSecondNumber => {
                let number = self.string_prototype_number(primitive)?;
                self.update_native_call_state_value(state, STRING_SECOND, number)?;
                self.finish_string_prototype_state(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Completes the observable `IsRegExp(searchString)` `@@match` read.
    pub(crate) fn resume_string_prototype(
        &mut self,
        continuation: NativeContinuation,
        stage: StringPrototypeStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            StringPrototypeStage::MatchGet => {
                let search = self.native_call_state_snapshot(state)?.values[STRING_FIRST];
                let is_regexp = if value.as_immediate() == Some(Immediate::Undefined) {
                    self.is_regexp_value(search)
                } else {
                    self.is_truthy_value(value)?
                };
                if is_regexp {
                    return Err(ExecutionError::NotObject(search));
                }
                self.begin_string_prototype_string(continuation.site(), state)
            }
        }
    }

    /// Converts the receiver with string hint before touching any method argument.
    fn begin_string_prototype_receiver(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[STRING_RECEIVER];
        if self.is_object_value(receiver) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringPrototypeReceiver,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                receiver,
                site.call_site,
            );
        }
        let receiver = self.primitive_to_string_value(receiver)?;
        self.update_native_call_state_value(state, STRING_RECEIVER, receiver)?;
        self.begin_string_prototype_first(site, state)
    }

    /// Selects the next exact conversion step from the closed operation identity.
    fn begin_string_prototype_first(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let operation = self.string_prototype_operation(state)?;
        match operation {
            StringPrototypeOperation::IsWellFormed | StringPrototypeOperation::ToWellFormed => {
                self.finish_string_prototype_state(site, state)
            }
            StringPrototypeOperation::IndexOf | StringPrototypeOperation::LastIndexOf => {
                self.begin_string_prototype_string(site, state)
            }
            StringPrototypeOperation::Includes
            | StringPrototypeOperation::StartsWith
            | StringPrototypeOperation::EndsWith => {
                self.begin_string_prototype_regexp_check(site, state)
            }
            _ => self.begin_string_prototype_number(site, state, false),
        }
    }

    /// Converts the search operand after receiver ToString has completed.
    fn begin_string_prototype_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let value = self.native_call_state_snapshot(state)?.values[STRING_FIRST];
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringPrototypeString,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        let value = self.primitive_to_string_value(value)?;
        self.update_native_call_state_value(state, STRING_FIRST, value)?;
        self.begin_string_prototype_after_first_string(site, state)
    }

    /// Advances search methods from the converted needle to their numeric position.
    fn begin_string_prototype_after_first_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let operation = self.string_prototype_operation(state)?;
        if operation == StringPrototypeOperation::EndsWith
            && self.native_call_state_snapshot(state)?.values[STRING_SECOND].as_immediate()
                == Some(Immediate::Undefined)
        {
            self.finish_string_prototype_state(site, state)
        } else {
            self.begin_string_prototype_number(site, state, true)
        }
    }

    /// Performs Proxy/accessor-aware `Get(search, @@match)` before search ToString.
    fn begin_string_prototype_regexp_check(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let search = self.native_call_state_snapshot(state)?.values[STRING_FIRST];
        if !self.is_object_value(search) {
            return self.begin_string_prototype_string(site, state);
        }
        let symbol = self
            .agent
            .well_known_symbols
            .r#match
            .expect("Symbol.match initializes before String containment methods");
        let key = self.property_key(symbol)?;
        let continuation = NativeContinuation::string_prototype(
            site,
            StringPrototypeStage::MatchGet,
            Value::from_heap_ref(state.raw()),
            search,
        );
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, search, search, key) {
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
        self.resume_string_prototype(continuation, StringPrototypeStage::MatchGet, value)
    }

    /// Converts either numeric operand with number hint, preserving source order.
    fn begin_string_prototype_number(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        second: bool,
    ) -> Result<(), ExecutionError> {
        let slot = if second { STRING_SECOND } else { STRING_FIRST };
        let value = self.native_call_state_snapshot(state)?.values[slot];
        let consumer = if second {
            ConversionConsumer::StringPrototypeSecondNumber
        } else {
            ConversionConsumer::StringPrototypeFirstNumber
        };
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                consumer,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        let number = self.string_prototype_number(value)?;
        self.update_native_call_state_value(state, slot, number)?;
        if second {
            self.finish_string_prototype_state(site, state)
        } else {
            self.begin_string_prototype_after_first_number(site, state)
        }
    }

    /// Handles optional second numbers and pad's conditional filler conversion.
    fn begin_string_prototype_after_first_number(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let operation = self.string_prototype_operation(state)?;
        match operation {
            StringPrototypeOperation::Slice | StringPrototypeOperation::Substring => {
                let end = self.native_call_state_snapshot(state)?.values[STRING_SECOND];
                if end.as_immediate() == Some(Immediate::Undefined) {
                    self.finish_string_prototype_state(site, state)
                } else {
                    self.begin_string_prototype_number(site, state, true)
                }
            }
            StringPrototypeOperation::PadStart | StringPrototypeOperation::PadEnd => {
                self.begin_string_prototype_pad_filler(site, state)
            }
            _ => self.finish_string_prototype_state(site, state),
        }
    }

    /// Skips filler access when ToLength(target) cannot grow the receiver.
    fn begin_string_prototype_pad_filler(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let source_length = self.string_value_length(pending.values[STRING_RECEIVER])?;
        let target = to_length(number_value(pending.values[STRING_FIRST])?);
        if target <= source_length
            || pending.values[STRING_SECOND].as_immediate() == Some(Immediate::Undefined)
        {
            return self.finish_string_prototype_state(site, state);
        }
        let filler = pending.values[STRING_SECOND];
        if self.is_object_value(filler) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::StringPrototypeFiller,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                filler,
                site.call_site,
            );
        }
        let filler = self.primitive_to_string_value(filler)?;
        self.update_native_call_state_value(state, STRING_SECOND, filler)?;
        self.finish_string_prototype_state(site, state)
    }

    /// Materializes the result after all observable conversions have completed.
    fn finish_string_prototype_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let operation = StringPrototypeOperation::from_u8(pending.count)
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.finish_primitive_string_operation(
            site,
            operation,
            pending.values[STRING_RECEIVER],
            pending.values[STRING_FIRST],
            pending.values[STRING_SECOND],
        )
    }

    /// Runs one conversion-free UTF-16 kernel and writes its result.
    fn finish_primitive_string_operation(
        &mut self,
        site: NativeContinuationSite,
        operation: StringPrototypeOperation,
        receiver: Value,
        first: Value,
        second: Value,
    ) -> Result<(), ExecutionError> {
        let result = match operation {
            StringPrototypeOperation::IsWellFormed => self.string_is_well_formed(receiver)?,
            StringPrototypeOperation::ToWellFormed => self.string_to_well_formed(receiver)?,
            _ => {
                let units = self.primitive_string_units(receiver)?;
                self.string_prototype_kernel(operation, receiver, &units, first, second)?
            }
        };
        self.write(site.caller_base, site.destination, result)
    }

    /// Normalizes primitive operands without allocating a slow-path state.
    fn finish_fast_string_operation(
        &mut self,
        site: NativeContinuationSite,
        operation: StringPrototypeOperation,
        receiver: Value,
        mut first: Value,
        mut second: Value,
    ) -> Result<(), ExecutionError> {
        match operation {
            StringPrototypeOperation::IsWellFormed | StringPrototypeOperation::ToWellFormed => {}
            StringPrototypeOperation::IndexOf | StringPrototypeOperation::LastIndexOf => {
                second = self.string_prototype_number(second)?;
            }
            StringPrototypeOperation::Includes | StringPrototypeOperation::StartsWith => {
                second = self.string_prototype_number(second)?;
            }
            StringPrototypeOperation::EndsWith => {
                if second.as_immediate() != Some(Immediate::Undefined) {
                    second = self.string_prototype_number(second)?;
                }
            }
            StringPrototypeOperation::Slice | StringPrototypeOperation::Substring => {
                first = self.string_prototype_number(first)?;
                if second.as_immediate() != Some(Immediate::Undefined) {
                    second = self.string_prototype_number(second)?;
                }
            }
            _ => first = self.string_prototype_number(first)?,
        }
        self.finish_primitive_string_operation(site, operation, receiver, first, second)
    }

    /// Executes algorithms over an already converted String and primitive operands only.
    fn string_prototype_kernel(
        &mut self,
        operation: StringPrototypeOperation,
        receiver: Value,
        units: &[u16],
        first: Value,
        second: Value,
    ) -> Result<Value, ExecutionError> {
        match operation {
            StringPrototypeOperation::CharAt => self.string_character_result(units, first, false),
            StringPrototypeOperation::CharCodeAt => Ok(code_unit_index(units, first)?.map_or_else(
                || Value::from_f64(f64::NAN),
                |index| Value::from_i32(i32::from(units[index])),
            )),
            StringPrototypeOperation::At => self.string_at_result(units, first),
            StringPrototypeOperation::CodePointAt => self.string_code_point_result(units, first),
            StringPrototypeOperation::Slice => self.string_slice_result(units, first, second),
            StringPrototypeOperation::Substring => {
                self.string_substring_result(units, first, second)
            }
            StringPrototypeOperation::IndexOf => {
                self.string_index_result(units, first, second, false)
            }
            StringPrototypeOperation::LastIndexOf => {
                self.string_index_result(units, first, second, true)
            }
            StringPrototypeOperation::Repeat => self.string_repeat_result(units, first),
            StringPrototypeOperation::PadStart => {
                self.string_pad_result(receiver, units, first, second, false)
            }
            StringPrototypeOperation::PadEnd => {
                self.string_pad_result(receiver, units, first, second, true)
            }
            StringPrototypeOperation::Includes => {
                self.string_contains_result(units, first, second, 0)
            }
            StringPrototypeOperation::StartsWith => {
                self.string_contains_result(units, first, second, 1)
            }
            StringPrototypeOperation::EndsWith => {
                self.string_contains_result(units, first, second, 2)
            }
            StringPrototypeOperation::IsWellFormed | StringPrototypeOperation::ToWellFormed => {
                unreachable!("well-formed operations use their existing allocation-aware kernels")
            }
        }
    }

    fn string_operation_is_primitive_fast(
        &mut self,
        operation: StringPrototypeOperation,
        receiver: Value,
        first: Value,
        second: Value,
    ) -> bool {
        if !self.is_string_value(receiver) {
            return false;
        }
        match operation {
            StringPrototypeOperation::IsWellFormed | StringPrototypeOperation::ToWellFormed => true,
            StringPrototypeOperation::IndexOf | StringPrototypeOperation::LastIndexOf => {
                self.is_string_value(first) && !self.is_object_value(second)
            }
            StringPrototypeOperation::Includes
            | StringPrototypeOperation::StartsWith
            | StringPrototypeOperation::EndsWith => {
                self.is_string_value(first) && !self.is_object_value(second)
            }
            StringPrototypeOperation::Slice | StringPrototypeOperation::Substring => {
                !self.is_object_value(first) && !self.is_object_value(second)
            }
            StringPrototypeOperation::PadStart | StringPrototypeOperation::PadEnd => false,
            _ => !self.is_object_value(first),
        }
    }

    fn string_prototype_number(&mut self, value: Value) -> Result<Value, ExecutionError> {
        self.convert_to_number(value)
    }

    fn string_prototype_operation(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<StringPrototypeOperation, ExecutionError> {
        StringPrototypeOperation::from_u8(self.native_call_state_snapshot(state)?.count)
            .ok_or(ExecutionError::MissingNativeContinuation)
    }

    /// Allocates the fixed state while the original call frame still roots every operand.
    fn allocate_string_prototype_state(
        &mut self,
        operation: StringPrototypeOperation,
        receiver: Value,
        first: Value,
        second: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = StringPrototypeRoots {
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
                values: [receiver, first, second, undefined, undefined],
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

fn number_value(value: Value) -> Result<f64, ExecutionError> {
    numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))
}

fn integer_or_infinity(value: Value) -> Result<f64, ExecutionError> {
    let number = number_value(value)?;
    Ok(if number.is_nan() || number == 0.0 {
        0.0
    } else if number.is_infinite() {
        number
    } else {
        number.trunc()
    })
}

fn to_length(number: f64) -> usize {
    if number.is_nan() || number <= 0.0 {
        0
    } else {
        number.floor().min(MAX_SAFE_INTEGER) as usize
    }
}

fn code_unit_index(units: &[u16], value: Value) -> Result<Option<usize>, ExecutionError> {
    let position = integer_or_infinity(value)?;
    if position < 0.0 || position >= units.len() as f64 {
        Ok(None)
    } else {
        Ok(Some(position as usize))
    }
}

impl Isolate {
    fn string_character_result(
        &mut self,
        units: &[u16],
        position: Value,
        undefined: bool,
    ) -> Result<Value, ExecutionError> {
        let Some(index) = code_unit_index(units, position)? else {
            return if undefined {
                Ok(Value::from_immediate(Immediate::Undefined))
            } else {
                self.allocate_runtime_string(
                    JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
                )
            };
        };
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units[index..=index])
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    fn string_at_result(
        &mut self,
        units: &[u16],
        position: Value,
    ) -> Result<Value, ExecutionError> {
        let relative = integer_or_infinity(position)?;
        let index = if relative >= 0.0 {
            relative
        } else {
            units.len() as f64 + relative
        };
        self.string_character_result(units, Value::from_f64(index), true)
    }

    fn string_code_point_result(
        &mut self,
        units: &[u16],
        position: Value,
    ) -> Result<Value, ExecutionError> {
        let Some(index) = code_unit_index(units, position)? else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        let first = units[index];
        let code_point = if let Some(&second) = units.get(index + 1)
            && (0xd800..=0xdbff).contains(&first)
            && (0xdc00..=0xdfff).contains(&second)
        {
            0x1_0000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00
        } else {
            u32::from(first)
        };
        Ok(Value::from_f64(f64::from(code_point)))
    }

    fn string_slice_result(
        &mut self,
        units: &[u16],
        start: Value,
        end: Value,
    ) -> Result<Value, ExecutionError> {
        let from = relative_index(integer_or_infinity(start)?, units.len());
        let to = if end.as_immediate() == Some(Immediate::Undefined) {
            units.len()
        } else {
            relative_index(integer_or_infinity(end)?, units.len())
        };
        let to = to.max(from);
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units[from..to])
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    fn string_substring_result(
        &mut self,
        units: &[u16],
        start: Value,
        end: Value,
    ) -> Result<Value, ExecutionError> {
        let start = clamp_index(integer_or_infinity(start)?, units.len());
        let end = if end.as_immediate() == Some(Immediate::Undefined) {
            units.len()
        } else {
            clamp_index(integer_or_infinity(end)?, units.len())
        };
        let (from, to) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units[from..to])
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    fn string_index_result(
        &mut self,
        haystack: &[u16],
        needle: Value,
        position: Value,
        reverse: bool,
    ) -> Result<Value, ExecutionError> {
        let needle = self.primitive_string_units(needle)?;
        let raw_position = number_value(position)?;
        let normalized = if reverse && raw_position.is_nan() {
            f64::INFINITY
        } else {
            integer_or_infinity(position)?
        };
        let start = clamp_index(normalized, haystack.len());
        let found = if reverse {
            let last = haystack.len().saturating_sub(needle.len()).min(start);
            (needle.len() <= haystack.len())
                .then(|| {
                    (0..=last)
                        .rev()
                        .find(|&index| haystack[index..index + needle.len()] == needle)
                })
                .flatten()
        } else if needle.len() <= haystack.len().saturating_sub(start) {
            (start..=haystack.len() - needle.len())
                .find(|&index| haystack[index..index + needle.len()] == needle)
        } else {
            None
        };
        Ok(found.map_or_else(
            || Value::from_i32(-1),
            |index| safe_integer_value(index as u64),
        ))
    }

    /// Implements includes/startsWith/endsWith after IsRegExp and all conversions completed.
    fn string_contains_result(
        &mut self,
        haystack: &[u16],
        needle: Value,
        position: Value,
        mode: u8,
    ) -> Result<Value, ExecutionError> {
        let needle = self.primitive_string_units(needle)?;
        let position = if mode == 2 && position.as_immediate() == Some(Immediate::Undefined) {
            haystack.len()
        } else {
            clamp_index(integer_or_infinity(position)?, haystack.len())
        };
        let matched = match mode {
            0 => {
                needle.len() <= haystack.len().saturating_sub(position)
                    && (position..=haystack.len() - needle.len())
                        .any(|index| haystack[index..index + needle.len()] == needle)
            }
            1 => {
                needle.len() <= haystack.len().saturating_sub(position)
                    && haystack[position..position + needle.len()] == needle
            }
            2 => {
                let start = position.saturating_sub(needle.len());
                needle.len() <= position && haystack[start..position] == needle
            }
            _ => unreachable!("String containment mode is closed"),
        };
        Ok(Value::from_immediate(if matched {
            Immediate::True
        } else {
            Immediate::False
        }))
    }

    fn string_repeat_result(
        &mut self,
        units: &[u16],
        count: Value,
    ) -> Result<Value, ExecutionError> {
        let count_number = integer_or_infinity(count)?;
        if count_number < 0.0 || !count_number.is_finite() {
            return Err(ExecutionError::InvalidStringRepeatCount(count));
        }
        let count = count_number as usize;
        let capacity = units
            .len()
            .checked_mul(count)
            .filter(|length| *length <= u32::MAX as usize)
            .ok_or(ExecutionError::InvalidStringLength)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for _ in 0..count {
            output.extend_from_slice(units);
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    fn string_pad_result(
        &mut self,
        receiver: Value,
        units: &[u16],
        target: Value,
        filler: Value,
        pad_end: bool,
    ) -> Result<Value, ExecutionError> {
        let target = to_length(number_value(target)?);
        if target <= units.len() {
            return Ok(receiver);
        }
        let fill = if filler.as_immediate() == Some(Immediate::Undefined) {
            vec![u16::from(b' ')]
        } else {
            self.primitive_string_units(filler)?
        };
        if fill.is_empty() {
            return Ok(receiver);
        }
        if target > u32::MAX as usize {
            return Err(ExecutionError::InvalidStringLength);
        }
        let fill_length = target - units.len();
        let mut output = Vec::new();
        output
            .try_reserve_exact(target)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        if !pad_end {
            append_repeated_units(&mut output, &fill, fill_length);
        }
        output.extend_from_slice(units);
        if pad_end {
            append_repeated_units(&mut output, &fill, fill_length);
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }
}

fn clamp_index(integer: f64, length: usize) -> usize {
    if integer <= 0.0 {
        0
    } else if integer >= length as f64 {
        length
    } else {
        integer as usize
    }
}

fn relative_index(integer: f64, length: usize) -> usize {
    if integer < 0.0 {
        (length as f64 + integer).clamp(0.0, length as f64) as usize
    } else {
        integer.min(length as f64) as usize
    }
}

fn append_repeated_units(output: &mut Vec<u16>, fill: &[u16], count: usize) {
    let full = count / fill.len();
    let remainder = count % fill.len();
    for _ in 0..full {
        output.extend_from_slice(fill);
    }
    output.extend_from_slice(&fill[..remainder]);
}
