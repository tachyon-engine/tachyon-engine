//! Primitive conversion, ToPrimitive continuation, and equality semantics.

mod equality;
mod exotic;
mod native_property_key;
mod numeric;
mod property_key;

use super::*;
pub(crate) use numeric::parse_number_code_units;

pub(crate) use numeric::{
    numeric_binary, numeric_binary_hot, numeric_binary_operation, numeric_bitwise_not,
    numeric_negate, numeric_relational, numeric_relational_hot, numeric_value, safe_integer_value,
};

struct RuntimeStringAllocationRoots<'a> {
    vm: VmRoots<'a>,
    retained: Value,
}

impl Trace for RuntimeStringAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.retained.trace(tracer);
    }
}

pub(super) enum ConversionCallbackResult {
    Suspended,
    Returned(Value),
}

pub(crate) use native_property_key::PendingNativePropertyKey;

impl Isolate {
    /// Implements ToIndex for builtin view offsets and lengths without host-width wrapping.
    pub(crate) fn ecma_to_index(&mut self, value: Value) -> Result<usize, ExecutionError> {
        if self.is_bigint_value(value) {
            return Err(ExecutionError::NotObject(value));
        }
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() || number == 0.0 {
            return Ok(0);
        }
        let integer = number.trunc();
        if !integer.is_finite() || integer < 0.0 || integer > MAX_SAFE_INTEGER as f64 {
            return Err(ExecutionError::InvalidArrayLength);
        }
        usize::try_from(integer as u64).map_err(|_| ExecutionError::InvalidArrayLength)
    }

    /// Executes one primitive constructor using the exact call argument window.
    pub(crate) fn primitive_constructor_value(
        &mut self,
        native: NativeFunction,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        match native {
            NativeFunction::StringConstructor => self.primitive_string_value(argument),
            NativeFunction::SymbolConstructor => self.allocate_symbol(
                argument.filter(|value| value.as_immediate() != Some(Immediate::Undefined)),
            ),
            NativeFunction::NumberConstructor => {
                let argument = argument.unwrap_or(Value::from_i32(0));
                self.convert_to_number(argument)
            }
            NativeFunction::BigIntConstructor => {
                let argument = argument.unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.bigint_constructor_primitive(argument)
            }
            NativeFunction::BooleanConstructor => {
                let argument = argument.unwrap_or(Value::from_immediate(Immediate::Undefined));
                Ok(Value::from_immediate(if self.is_truthy_value(argument)? {
                    Immediate::True
                } else {
                    Immediate::False
                }))
            }
            _ => Err(ExecutionError::NonCallable(Value::from_immediate(
                Immediate::Undefined,
            ))),
        }
    }

    /// Converts one already-primitive String constructor argument into its canonical string value.
    pub(crate) fn primitive_string_value(
        &mut self,
        argument: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let Some(argument) = argument else {
            return self.allocate_runtime_string(
                JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
            );
        };
        if self.is_string_value(argument) {
            return Ok(argument);
        }
        let mut units = Vec::new();
        if self.is_symbol_value(argument) {
            self.append_symbol_string_units(argument, &mut units)?;
        } else {
            self.append_primitive_string_units(argument, &mut units)?;
        }
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Applies ordinary ToString to a primitive, rejecting Symbol unlike the explicit String call.
    pub(crate) fn primitive_to_string_value(
        &mut self,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        if self.is_string_value(value) {
            return Ok(value);
        }
        let mut units = Vec::new();
        self.append_primitive_string_units(value, &mut units)?;
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Converts one primitive String argument while retaining an earlier managed edge.
    pub(crate) fn primitive_string_value_retaining(
        &mut self,
        argument: Option<Value>,
        retained: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        let Some(argument) = argument else {
            return self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
                retained,
            );
        };
        if self.is_string_value(argument) {
            return Ok((argument, retained));
        }
        let mut units = Vec::new();
        if self.is_symbol_value(argument) {
            self.append_symbol_string_units(argument, &mut units)?;
        } else {
            self.append_primitive_string_units(argument, &mut units)?;
        }
        self.allocate_runtime_string_retaining(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
            retained,
        )
    }

    /// Allocates one unique Symbol primitive while retaining its optional description as a GC edge.
    pub(crate) fn allocate_symbol(
        &mut self,
        description: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        self.allocate_symbol_with_registration(description, false)
    }

    /// Allocates one Symbol while recording whether the ECMAScript global registry owns it.
    pub(crate) fn allocate_registered_symbol(
        &mut self,
        description: Value,
    ) -> Result<Value, ExecutionError> {
        self.allocate_symbol_with_registration(Some(description), true)
    }

    fn allocate_symbol_with_registration(
        &mut self,
        description: Option<Value>,
        registered: bool,
    ) -> Result<Value, ExecutionError> {
        let serial = self.next_symbol_serial;
        let next_serial = serial
            .get()
            .checked_add(1)
            .and_then(NonZeroU32::new)
            .ok_or(ExecutionError::SymbolIdExhausted)?;
        let roots = &mut SymbolAllocationRoots {
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
            description,
        };
        let symbol = self
            .heap
            .try_allocate_with_gc(
                self.types.symbol,
                0,
                0,
                SymbolValue {
                    serial,
                    description: roots.description,
                    registered,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.next_symbol_serial = next_serial;
        self.realm
            .retain_construction_value(Value::from_heap_ref(symbol.raw()))
    }

    /// Starts one conversion consumer, suspending only when its argument requires a JS callback.
    pub(crate) fn dispatch_conversion_native(
        &mut self,
        native: NativeFunction,
        site: &CallSite,
        construct: bool,
    ) -> Result<(), ExecutionError> {
        let conversion_native = ConversionNativeFunction::from_native(native)
            .expect("only conversion natives enter the resumable conversion path");
        let consumer = if construct {
            ConversionConsumer::NativeConstruct(conversion_native)
        } else {
            ConversionConsumer::NativeCall(conversion_native)
        };
        let argument = self.call_argument(site, 0)?;
        let receiver = match native {
            NativeFunction::StringConstructor => {
                if construct {
                    site.new_target
                } else {
                    Value::from_immediate(Immediate::Undefined)
                }
            }
            NativeFunction::SymbolConstructor | NativeFunction::SymbolFor => {
                Value::from_immediate(Immediate::Undefined)
            }
            NativeFunction::StringIterator => Value::from_immediate(Immediate::Undefined),
            NativeFunction::NumberToExponential
            | NativeFunction::NumberToFixed
            | NativeFunction::NumberToPrecision
            | NativeFunction::NumberToString => self.this_number_value(site.this_value)?,
            NativeFunction::NumberConstructor => {
                if construct {
                    site.new_target
                } else {
                    Value::from_immediate(Immediate::Undefined)
                }
            }
            NativeFunction::BigIntConstructor => Value::from_immediate(Immediate::Undefined),
            NativeFunction::BigIntToString => self.this_bigint_value(site.this_value)?,
            NativeFunction::BigIntAsIntN | NativeFunction::BigIntAsUintN => self
                .call_argument(site, 1)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            NativeFunction::GlobalIsFinite
            | NativeFunction::GlobalIsNaN
            | NativeFunction::GlobalParseFloat
            | NativeFunction::GlobalParseInt
            | NativeFunction::GlobalDecodeUri
            | NativeFunction::GlobalDecodeUriComponent
            | NativeFunction::GlobalEncodeUri
            | NativeFunction::GlobalEncodeUriComponent
            | NativeFunction::DateParse => Value::from_immediate(Immediate::Undefined),
            _ => unreachable!("only argument conversion consumers enter this dispatch path"),
        };
        self.dispatch_native_conversion_operand(consumer, site, receiver, argument)
    }

    /// Starts ToString over a String prototype method's receiver without recursive interpretation.
    pub(crate) fn dispatch_string_receiver_conversion(
        &mut self,
        native: NativeFunction,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        if matches!(
            receiver.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let conversion_native = ConversionNativeFunction::from_native(native)
            .expect("only receiver conversion natives enter the resumable path");
        self.dispatch_native_conversion_operand(
            ConversionConsumer::NativeCall(conversion_native),
            site,
            Value::from_immediate(Immediate::Undefined),
            Some(receiver),
        )
    }

    /// Publishes a conversion continuation only when the selected operand is an object.
    fn dispatch_native_conversion_operand(
        &mut self,
        consumer: ConversionConsumer,
        site: &CallSite,
        receiver: Value,
        argument: Option<Value>,
    ) -> Result<(), ExecutionError> {
        if let Some(object) = argument
            && self.is_object_value(object)
        {
            let continuation = ConversionContinuation {
                site: NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                consumer,
                receiver,
                object,
                stage: ToPrimitiveStage::Exotic,
                callback_stage: ConversionCallbackStage::MethodCall,
            };
            return self.advance_native_conversion(continuation, None);
        }
        if let Some(native) = consumer.native()
            && matches!(
                native,
                NativeFunction::BigIntAsIntN | NativeFunction::BigIntAsUintN
            )
        {
            return self.resume_bigint_as_n_value(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                native == NativeFunction::BigIntAsIntN,
                argument.unwrap_or(Value::from_immediate(Immediate::Undefined)),
                receiver,
            );
        }
        let value = self.finish_conversion_consumer(consumer, receiver, argument)?;
        self.write(site.caller_base, site.destination, value)
    }

    /// Continues asIntN/asUintN with ToBigInt after the leading ToIndex has completed.
    fn resume_bigint_as_n_value(
        &mut self,
        site: NativeContinuationSite,
        signed: bool,
        bits: Value,
        bigint: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(bigint) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::BigIntAsNValue(signed),
                site.caller_base,
                site.destination,
                bits,
                bigint,
                site.call_site,
            );
        }
        let result = self.finish_bigint_as_n(bits, bigint, signed)?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Starts a cold object conversion while tracing one optional pending operand.
    #[cold]
    #[inline(never)]
    pub(crate) fn dispatch_object_primitive_conversion(
        &mut self,
        consumer: ConversionConsumer,
        caller_base: u32,
        destination: u32,
        pending: Value,
        object: Value,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        debug_assert!(self.is_object_value(object));
        debug_assert!(consumer.is_resumable_operation());
        self.advance_native_conversion(
            ConversionContinuation {
                site: NativeContinuationSite {
                    caller_base,
                    destination,
                    call_site,
                },
                consumer,
                receiver: pending,
                object,
                stage: ToPrimitiveStage::Exotic,
                callback_stage: ConversionCallbackStage::MethodCall,
            },
            None,
        )
    }

    /// Advances ordinary ToPrimitive without recursively entering the interpreter.
    pub(crate) fn advance_native_conversion(
        &mut self,
        mut continuation: ConversionContinuation,
        mut returned: Option<Value>,
    ) -> Result<(), ExecutionError> {
        let mut resolved_method = None;
        loop {
            if let Some(value) = returned.take() {
                if continuation.callback_stage == ConversionCallbackStage::Getter {
                    continuation.callback_stage = ConversionCallbackStage::MethodCall;
                    resolved_method = Some(value);
                } else if !self.is_object_value(value) {
                    if let ConversionConsumer::BuiltinPropertyKey(consumer) = continuation.consumer
                    {
                        let pending = self.pending_native_property_key(continuation.receiver)?;
                        return self.finish_builtin_property_key(
                            continuation.site,
                            consumer,
                            pending,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArraySetLengthUint32
                            | ConversionConsumer::ArraySetLengthNumber
                    ) {
                        let state =
                            self.pending_property_descriptor_reference(continuation.receiver)?;
                        return self.resume_array_set_length_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::JsonStringifyNumberSpace
                            | ConversionConsumer::JsonStringifyStringSpace
                            | ConversionConsumer::JsonStringifyNumberValue
                            | ConversionConsumer::JsonStringifyStringValue
                            | ConversionConsumer::JsonStringifyArrayLength
                            | ConversionConsumer::JsonStringifyPropertyListLength
                            | ConversionConsumer::JsonStringifyPropertyListString
                    ) {
                        let state = self.pending_json_stringify_reference(continuation.receiver)?;
                        return match continuation.consumer {
                            ConversionConsumer::JsonStringifyNumberSpace => {
                                let primitive = self.convert_to_number(value)?;
                                self.resume_json_space_conversion(
                                    continuation.site,
                                    state,
                                    primitive,
                                )
                            }
                            ConversionConsumer::JsonStringifyStringSpace => {
                                let primitive = self.primitive_string_value(Some(value))?;
                                self.resume_json_space_conversion(
                                    continuation.site,
                                    state,
                                    primitive,
                                )
                            }
                            ConversionConsumer::JsonStringifyNumberValue => {
                                let primitive = self.convert_to_number(value)?;
                                self.resume_json_value_conversion(
                                    continuation.site,
                                    state,
                                    primitive,
                                )
                            }
                            ConversionConsumer::JsonStringifyStringValue => {
                                let primitive = self.primitive_string_value(Some(value))?;
                                self.resume_json_value_conversion(
                                    continuation.site,
                                    state,
                                    primitive,
                                )
                            }
                            ConversionConsumer::JsonStringifyArrayLength => self
                                .resume_json_array_length_conversion(
                                    continuation.site,
                                    state,
                                    value,
                                ),
                            ConversionConsumer::JsonStringifyPropertyListLength => self
                                .resume_json_property_list_length_conversion(
                                    continuation.site,
                                    state,
                                    value,
                                ),
                            ConversionConsumer::JsonStringifyPropertyListString => {
                                let primitive = self.primitive_string_value(Some(value))?;
                                self.resume_json_property_list_element_conversion(
                                    continuation.site,
                                    state,
                                    primitive,
                                )
                            }
                            _ => unreachable!("matched JSON conversion consumer"),
                        };
                    }
                    if continuation.consumer == ConversionConsumer::DynamicFunctionArgument {
                        let state =
                            self.pending_dynamic_function_reference(continuation.receiver)?;
                        return self.resume_dynamic_function_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::StringConcatElement {
                        return self.resume_string_concat_conversion(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::StringRawLength
                            | ConversionConsumer::StringRawLiteral
                            | ConversionConsumer::StringRawSubstitution
                    ) {
                        let state = self.pending_string_raw_reference(continuation.receiver)?;
                        return self.resume_string_raw_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::JsonParseText {
                        let text = self.primitive_string_value(Some(value))?;
                        return self.finish_json_parse_text(
                            continuation.site,
                            continuation.receiver,
                            text,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::AddLeft {
                        let left = value;
                        let right = continuation.receiver;
                        if self.is_object_value(right) {
                            continuation.consumer = ConversionConsumer::AddRight;
                            continuation.receiver = left;
                            continuation.object = right;
                            continuation.stage = ToPrimitiveStage::Exotic;
                            continue;
                        }
                        let result = self.add_primitive_values(left, right)?;
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::AddRight {
                        let result = self.add_primitive_values(continuation.receiver, value)?;
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if let ConversionConsumer::RelationalLeft(opcode) = continuation.consumer {
                        let left = value;
                        let right = continuation.receiver;
                        if self.is_object_value(right) {
                            continuation.consumer = ConversionConsumer::RelationalRight(opcode);
                            continuation.receiver = left;
                            continuation.object = right;
                            continuation.stage = ToPrimitiveStage::Exotic;
                            continue;
                        }
                        let result = self.relational_primitive_values(opcode, left, right)?;
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if let ConversionConsumer::RelationalRight(opcode) = continuation.consumer {
                        let result =
                            self.relational_primitive_values(opcode, continuation.receiver, value)?;
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if let ConversionConsumer::BinaryLeft(opcode) = continuation.consumer {
                        let left = value;
                        let right = continuation.receiver;
                        if self.is_object_value(right) {
                            continuation.consumer = ConversionConsumer::BinaryRight(opcode);
                            continuation.receiver = left;
                            continuation.object = right;
                            continuation.stage = ToPrimitiveStage::Exotic;
                            continue;
                        }
                        let result =
                            self.numeric_primitive_binary_operation(opcode, left, right)?;
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if let ConversionConsumer::BinaryRight(opcode) = continuation.consumer {
                        let result = self.numeric_primitive_binary_operation(
                            opcode,
                            continuation.receiver,
                            value,
                        )?;
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if let ConversionConsumer::ArrayBufferTransferLength(to_fixed_length) =
                        continuation.consumer
                    {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_array_buffer_transfer_conversion(
                            continuation.site,
                            state,
                            to_fixed_length,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ErrorConstructorMessage
                            | ConversionConsumer::ErrorToStringName
                            | ConversionConsumer::ErrorToStringMessage
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        let string = self.with_error_state_root(
                            NativeContinuation::conversion(continuation),
                            |isolate| isolate.error_message_string(value),
                        )?;
                        return match continuation.consumer {
                            ConversionConsumer::ErrorConstructorMessage => {
                                self.finish_error_message(continuation.site, state, string)
                            }
                            ConversionConsumer::ErrorToStringName => {
                                self.finish_error_to_string_name(continuation.site, state, string)
                            }
                            ConversionConsumer::ErrorToStringMessage => self
                                .finish_error_to_string_message(continuation.site, state, string),
                            _ => unreachable!("matched Error string conversion consumer"),
                        };
                    }
                    if continuation.consumer == ConversionConsumer::ArrayRemoveLength {
                        let state = self.pending_array_remove_reference(continuation.receiver)?;
                        return self.resume_array_remove_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::TypedArrayByteOffset
                            | ConversionConsumer::TypedArrayLength
                            | ConversionConsumer::TypedArrayElement
                    ) {
                        let state =
                            self.pending_typed_array_construction_reference(continuation.receiver)?;
                        return self.resume_typed_array_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::TypedArrayAtIndex {
                        return self.resume_typed_array_at_conversion(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::TypedArrayWithIndex
                            | ConversionConsumer::TypedArrayWithValue
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_with_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::TypedArrayIndexSet {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_index_set_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::TypedArrayIncludesFromIndex {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_includes_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::TypedArrayFillValue
                            | ConversionConsumer::TypedArrayFillStart
                            | ConversionConsumer::TypedArrayFillEnd
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_fill_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::TypedArrayCopyWithinTarget
                            | ConversionConsumer::TypedArrayCopyWithinStart
                            | ConversionConsumer::TypedArrayCopyWithinEnd
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_copy_within_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::TypedArraySearchFromIndex {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_search_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::TypedArraySetOffset
                            | ConversionConsumer::TypedArraySetLength
                            | ConversionConsumer::TypedArraySetElement
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_set_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::TypedArrayJoinSeparator {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_join_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::TypedArraySliceStart
                            | ConversionConsumer::TypedArraySliceEnd
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_slice_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::TypedArraySubarrayStart
                            | ConversionConsumer::TypedArraySubarrayEnd
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_typed_array_subarray_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArrayInsertLength {
                        let state = self.pending_array_insert_reference(continuation.receiver)?;
                        return self.resume_array_insert_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArrayReverseLength {
                        let state = self.pending_array_reverse_reference(continuation.receiver)?;
                        return self.resume_array_reverse_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayFillLength
                            | ConversionConsumer::ArrayFillStart
                            | ConversionConsumer::ArrayFillEnd
                    ) {
                        let state = self.pending_array_fill_reference(continuation.receiver)?;
                        return self.resume_array_fill_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayJoinLength
                            | ConversionConsumer::ArrayJoinSeparator
                            | ConversionConsumer::ArrayJoinElement
                    ) {
                        let state = self.pending_array_join_reference(continuation.receiver)?;
                        return self.resume_array_join_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::StringSplitReceiver
                            | ConversionConsumer::StringSplitLimit
                            | ConversionConsumer::StringSplitSeparator
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_string_split_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::StringReplaceAllFlags
                            | ConversionConsumer::StringReplaceAllReceiver
                            | ConversionConsumer::StringReplaceAllSearch
                            | ConversionConsumer::StringReplaceAllReplacement
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_string_replace_all_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::StringPrototypeReceiver
                            | ConversionConsumer::StringPrototypeString
                            | ConversionConsumer::StringPrototypeFiller
                            | ConversionConsumer::StringPrototypeFirstNumber
                            | ConversionConsumer::StringPrototypeSecondNumber
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_string_prototype_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::RegExpTestInput {
                        return self.resume_regexp_test_conversion(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::RegExpSearchInput {
                        return self.resume_regexp_search_input_conversion(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::RegExpReplaceResult {
                        let state = self.pending_regexp_replace_reference(continuation.receiver)?;
                        return self.resume_regexp_replace_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::RegExpStringIteratorMatch {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_regexp_string_iterator_match_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::RegExpStringIteratorLastIndex {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_regexp_string_iterator_last_index_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::StringSearchReceiver
                            | ConversionConsumer::StringSearchPattern
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_string_search_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::RegExpExecInput {
                        return self.resume_regexp_exec_conversion(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::RegExpLastIndex {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_regexp_last_index_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayCopyWithinLength
                            | ConversionConsumer::ArrayCopyWithinTarget
                            | ConversionConsumer::ArrayCopyWithinStart
                            | ConversionConsumer::ArrayCopyWithinEnd
                    ) {
                        let state =
                            self.pending_array_copy_within_reference(continuation.receiver)?;
                        return self.resume_array_copy_within_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::DateNumericArgument {
                        return self.resume_date_numeric_arguments(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::DateConstructSingle {
                        return self.resume_single_date_constructor(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayToSortedLength
                            | ConversionConsumer::ArrayToSortedCompareResult
                            | ConversionConsumer::ArrayToSortedLeftString
                            | ConversionConsumer::ArrayToSortedRightString
                    ) {
                        let state =
                            self.pending_array_to_sorted_reference(continuation.receiver)?;
                        return self.resume_array_to_sorted_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::DateToJson {
                        return self.resume_date_to_json_after_primitive(
                            continuation.site,
                            continuation.receiver,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArrayLength {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_array_for_each_after_length_primitive(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArraySearchIndex {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_array_search_after_index_primitive(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(continuation.consumer, ConversionConsumer::ArrayConcatLength) {
                        let state = self.pending_array_concat_reference(continuation.receiver)?;
                        return self.resume_array_concat_length_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayFlatLength
                            | ConversionConsumer::ArrayFlatDepth
                            | ConversionConsumer::ArrayFlatElementLength
                    ) {
                        let state = self.pending_array_flat_reference(continuation.receiver)?;
                        return self.resume_array_flat_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayFlatMapLength
                            | ConversionConsumer::ArrayFlatMapInnerLength
                    ) {
                        let state = self.pending_array_flat_map_reference(continuation.receiver)?;
                        return self.resume_array_flat_map_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArrayStaticLength {
                        let state = self.pending_array_static_reference(continuation.receiver)?;
                        return self.resume_array_from_length_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayCopyLength
                            | ConversionConsumer::ArrayCopyIndex
                            | ConversionConsumer::ArrayCopyStart
                            | ConversionConsumer::ArrayCopyDeleteCount
                    ) {
                        let state = self.pending_array_copy_reference(continuation.receiver)?;
                        return self.resume_array_copy_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayCopyWithinLength
                            | ConversionConsumer::ArrayCopyWithinTarget
                            | ConversionConsumer::ArrayCopyWithinStart
                            | ConversionConsumer::ArrayCopyWithinEnd
                    ) {
                        let state =
                            self.pending_array_copy_within_reference(continuation.receiver)?;
                        return self.resume_array_copy_within_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArraySliceLength
                            | ConversionConsumer::ArraySliceStart
                            | ConversionConsumer::ArraySliceEnd
                    ) {
                        let state = self.pending_array_slice_reference(continuation.receiver)?;
                        return self.resume_array_slice_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayBufferSliceStart
                            | ConversionConsumer::ArrayBufferSliceEnd
                    ) {
                        let state = self.native_call_state_reference(continuation.receiver)?;
                        return self.resume_array_buffer_slice_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArraySpliceLength
                            | ConversionConsumer::ArraySpliceStart
                            | ConversionConsumer::ArraySpliceDeleteCount
                    ) {
                        let state = self.pending_array_splice_reference(continuation.receiver)?;
                        return self.resume_array_splice_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArrayRemoveLength {
                        let state = self.pending_array_remove_reference(continuation.receiver)?;
                        return self.resume_array_remove_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArrayInsertLength {
                        let state = self.pending_array_insert_reference(continuation.receiver)?;
                        return self.resume_array_insert_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if continuation.consumer == ConversionConsumer::ArrayReverseLength {
                        let state = self.pending_array_reverse_reference(continuation.receiver)?;
                        return self.resume_array_reverse_conversion(
                            continuation.site,
                            state,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayFillLength
                            | ConversionConsumer::ArrayFillStart
                            | ConversionConsumer::ArrayFillEnd
                    ) {
                        let state = self.pending_array_fill_reference(continuation.receiver)?;
                        return self.resume_array_fill_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if matches!(
                        continuation.consumer,
                        ConversionConsumer::ArrayJoinLength
                            | ConversionConsumer::ArrayJoinSeparator
                            | ConversionConsumer::ArrayJoinElement
                    ) {
                        let state = self.pending_array_join_reference(continuation.receiver)?;
                        return self.resume_array_join_conversion(
                            continuation.site,
                            state,
                            continuation.consumer,
                            value,
                        );
                    }
                    if let Some(native) = continuation.consumer.native()
                        && matches!(
                            native,
                            NativeFunction::BigIntAsIntN | NativeFunction::BigIntAsUintN
                        )
                    {
                        return self.resume_bigint_as_n_value(
                            continuation.site,
                            native == NativeFunction::BigIntAsIntN,
                            value,
                            continuation.receiver,
                        );
                    }
                    let result = self.finish_conversion_consumer(
                        continuation.consumer,
                        continuation.receiver,
                        Some(value),
                    )?;
                    return self.write(
                        continuation.site.caller_base,
                        continuation.site.destination,
                        result,
                    );
                } else if continuation.stage == ToPrimitiveStage::Exotic {
                    return Err(ExecutionError::NotObject(continuation.object));
                } else {
                    let Some(stage) =
                        next_to_primitive_stage(continuation.consumer, continuation.stage)
                    else {
                        return Err(ExecutionError::NotObject(continuation.object));
                    };
                    continuation.stage = stage;
                }
            }
            let method = if let Some(method) = resolved_method.take() {
                Some(method)
            } else {
                let key = match continuation.stage {
                    ToPrimitiveStage::Exotic => {
                        let symbol = self
                            .agent
                            .well_known_symbols
                            .to_primitive
                            .expect("well-known symbols initialize before execution");
                        self.property_key(symbol)?
                    }
                    ToPrimitiveStage::ValueOf => self.intern_intrinsic_name(b"valueOf")?.into(),
                    ToPrimitiveStage::ToString => self.intern_intrinsic_name(b"toString")?.into(),
                };
                self.push_native_conversion(continuation)?;
                let property = match self.resolve_conversion_property_read(continuation.object, key)
                {
                    Ok(property) => property,
                    Err(error) => {
                        self.pop_native_conversion()?;
                        return Err(error);
                    }
                };
                continuation = self.pop_native_conversion()?;
                match property {
                    PropertyRead::Missing => None,
                    PropertyRead::Data(method) => Some(method),
                    PropertyRead::Accessor(getter)
                        if getter.as_immediate() == Some(Immediate::Undefined) =>
                    {
                        None
                    }
                    PropertyRead::Accessor(getter) => {
                        continuation.callback_stage = ConversionCallbackStage::Getter;
                        match self.call_conversion_callback(continuation, getter, None)? {
                            ConversionCallbackResult::Suspended => return Ok(()),
                            ConversionCallbackResult::Returned(value) => {
                                returned = Some(value);
                                continue;
                            }
                        }
                    }
                }
            };
            let Some(method) = method else {
                let Some(stage) =
                    next_to_primitive_stage(continuation.consumer, continuation.stage)
                else {
                    return Err(ExecutionError::NotObject(continuation.object));
                };
                continuation.stage = stage;
                continue;
            };
            if is_nullish(method) && continuation.stage == ToPrimitiveStage::Exotic {
                continuation.stage =
                    next_to_primitive_stage(continuation.consumer, continuation.stage)
                        .expect("the exotic stage always has an ordinary fallback");
                continue;
            }
            if self.resolve_function_object(method).is_err() {
                if continuation.stage == ToPrimitiveStage::Exotic {
                    return Err(ExecutionError::NonCallable(method));
                }
                let Some(stage) =
                    next_to_primitive_stage(continuation.consumer, continuation.stage)
                else {
                    return Err(ExecutionError::NotObject(continuation.object));
                };
                continuation.stage = stage;
                continue;
            }
            continuation.callback_stage = ConversionCallbackStage::MethodCall;
            let argument = (continuation.stage == ToPrimitiveStage::Exotic).then(|| {
                self.realm
                    .primitive_hint_strings
                    .get(continuation.consumer.preferred_type())
            });
            match self.call_conversion_callback(continuation, method, argument)? {
                ConversionCallbackResult::Suspended => return Ok(()),
                ConversionCallbackResult::Returned(value) => returned = Some(value),
            }
        }
    }

    /// Follows Proxy targets only when their `get` trap is observably absent.
    fn resolve_conversion_property_read(
        &mut self,
        mut target: Value,
        key: PropertyKey,
    ) -> Result<PropertyRead, ExecutionError> {
        loop {
            match self.resolve_property_read_until_proxy(target, key)? {
                PropertyReadResolution::Read(read) => return Ok(read),
                PropertyReadResolution::Proxy(proxy) => {
                    let snapshot = self.proxy_snapshot(proxy)?;
                    if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                        return Err(ExecutionError::ProxyRevoked);
                    }
                    let get = self.intern_intrinsic_name(b"get")?;
                    let trap = self.resolve_property_read(snapshot.handler, get.into())?;
                    let absent = match trap {
                        PropertyRead::Missing => true,
                        PropertyRead::Data(value) => is_nullish(value),
                        PropertyRead::Accessor(getter) => {
                            getter.as_immediate() == Some(Immediate::Undefined)
                        }
                    };
                    if !absent {
                        return Err(ExecutionError::NotObject(proxy));
                    }
                    target = snapshot.target;
                }
            }
        }
    }

    /// Calls one getter or conversion method and reports whether it published a JavaScript frame.
    fn call_conversion_callback(
        &mut self,
        continuation: ConversionContinuation,
        callee: Value,
        argument: Option<Value>,
    ) -> Result<ConversionCallbackResult, ExecutionError> {
        if let Some(argument) = argument {
            return self.call_exotic_conversion_callback(continuation, callee, argument);
        }
        self.push_native_conversion(continuation)?;
        let frame_depth = self.fiber.frames.len();
        let call_result = self.call(CallSite {
            caller_base: continuation.site.caller_base,
            destination: continuation.site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: continuation.object,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: continuation.site.call_site,
        });
        if let Err(error) = call_result {
            self.pop_native_conversion()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a suspended conversion callback publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(ConversionCallbackResult::Suspended);
        }
        let continuation = self.pop_native_conversion()?;
        self.read(continuation.site.caller_base, continuation.site.destination)
            .map(ConversionCallbackResult::Returned)
    }

    /// Pushes one typed conversion sentinel with uniform completion-limit error mapping.
    fn push_native_conversion(
        &mut self,
        continuation: ConversionContinuation,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::conversion(continuation))
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })
    }

    /// Removes the exact native sentinel published before a callback call attempt.
    #[inline]
    pub(crate) fn pop_native_continuation(&mut self) -> Result<NativeContinuation, ExecutionError> {
        self.fiber
            .completions
            .pop_native()
            .ok_or(ExecutionError::MissingNativeContinuation)
    }

    /// Removes one conversion sentinel after its synchronous method call or failed lookup.
    #[inline]
    fn pop_native_conversion(&mut self) -> Result<ConversionContinuation, ExecutionError> {
        self.pop_native_continuation()?
            .as_conversion()
            .ok_or(ExecutionError::MissingNativeContinuation)
    }

    /// Completes one native consumer after its optional argument has become the required primitive.
    fn finish_conversion_consumer(
        &mut self,
        consumer: ConversionConsumer,
        receiver: Value,
        argument: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let Some(native) = consumer.native() else {
            let Some(argument) = argument else {
                return Err(ExecutionError::MissingNativeContinuation);
            };
            return Ok(match consumer {
                ConversionConsumer::ToNumber => self.convert_to_number(argument)?,
                ConversionConsumer::ToString => self.primitive_to_string_value(argument)?,
                ConversionConsumer::StringConcatElement => {
                    unreachable!("String concat conversion resumes inside its state machine")
                }
                ConversionConsumer::StringRawLength
                | ConversionConsumer::StringRawLiteral
                | ConversionConsumer::StringRawSubstitution => {
                    unreachable!("String.raw conversion resumes inside its state machine")
                }
                ConversionConsumer::Negate => self.numeric_primitive_negate(argument)?,
                ConversionConsumer::BitwiseNot => self.numeric_primitive_bitwise_not(argument)?,
                ConversionConsumer::BinaryLeft(_) | ConversionConsumer::BinaryRight(_) => {
                    unreachable!("binary consumers finish inside the conversion state machine")
                }
                ConversionConsumer::AddLeft | ConversionConsumer::AddRight => {
                    unreachable!("Add consumers finish inside the conversion state machine")
                }
                ConversionConsumer::RelationalLeft(_) | ConversionConsumer::RelationalRight(_) => {
                    unreachable!("relational consumers finish inside the conversion state machine")
                }
                ConversionConsumer::Equality(opcode) => {
                    let equal = self.loose_equal_values(argument, receiver)?;
                    Value::from_immediate(if equal == (opcode == Opcode::LooseEqual) {
                        Immediate::True
                    } else {
                        Immediate::False
                    })
                }
                ConversionConsumer::BigIntAsNValue(signed) => {
                    self.finish_bigint_as_n(receiver, argument, signed)?
                }
                ConversionConsumer::ToPropertyKey => argument,
                ConversionConsumer::BuiltinPropertyKey(_) => {
                    unreachable!("builtin property-key consumers finish inside the state machine")
                }
                ConversionConsumer::ArraySetLengthUint32
                | ConversionConsumer::ArraySetLengthNumber => {
                    unreachable!("ArraySetLength conversions finish inside the state machine")
                }
                ConversionConsumer::ErrorConstructorMessage
                | ConversionConsumer::ErrorToStringName
                | ConversionConsumer::ErrorToStringMessage => {
                    unreachable!("Error messages finish inside the conversion state machine")
                }
                ConversionConsumer::DynamicFunctionArgument => {
                    unreachable!("dynamic Function conversion resumes inside its state machine")
                }
                ConversionConsumer::DateConstructSingle
                | ConversionConsumer::DateNumericArgument => {
                    unreachable!("Date construction finishes inside the conversion state machine")
                }
                ConversionConsumer::DateToPrimitiveString
                | ConversionConsumer::DateToPrimitiveNumber => argument,
                ConversionConsumer::DateToJson => {
                    unreachable!("Date toJSON resumes inside the conversion state machine")
                }
                ConversionConsumer::JsonParseText => {
                    unreachable!("JSON parse text resumes inside the conversion state machine")
                }
                ConversionConsumer::JsonStringifyNumberSpace
                | ConversionConsumer::JsonStringifyStringSpace
                | ConversionConsumer::JsonStringifyNumberValue
                | ConversionConsumer::JsonStringifyStringValue
                | ConversionConsumer::JsonStringifyArrayLength
                | ConversionConsumer::JsonStringifyPropertyListLength
                | ConversionConsumer::JsonStringifyPropertyListString => {
                    unreachable!("JSON conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayLength => {
                    unreachable!("Array length resumes inside the conversion state machine")
                }
                ConversionConsumer::ArraySearchIndex => {
                    unreachable!("Array search index resumes inside the conversion state machine")
                }
                ConversionConsumer::ArrayConcatLength => {
                    unreachable!("Array concat length resumes inside its state machine")
                }
                ConversionConsumer::ArrayFlatLength
                | ConversionConsumer::ArrayFlatDepth
                | ConversionConsumer::ArrayFlatElementLength => {
                    unreachable!("Array flat conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayFlatMapLength
                | ConversionConsumer::ArrayFlatMapInnerLength => {
                    unreachable!("Array flatMap conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayStaticLength => {
                    unreachable!("Array static length resumes inside its state machine")
                }
                ConversionConsumer::ArrayCopyLength
                | ConversionConsumer::ArrayCopyIndex
                | ConversionConsumer::ArrayCopyStart
                | ConversionConsumer::ArrayCopyDeleteCount => {
                    unreachable!("Array copy conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayCopyWithinLength
                | ConversionConsumer::ArrayCopyWithinTarget
                | ConversionConsumer::ArrayCopyWithinStart
                | ConversionConsumer::ArrayCopyWithinEnd => {
                    unreachable!("Array copyWithin conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayToSortedLength
                | ConversionConsumer::ArrayToSortedCompareResult
                | ConversionConsumer::ArrayToSortedLeftString
                | ConversionConsumer::ArrayToSortedRightString => {
                    unreachable!("Array toSorted conversion resumes inside its state machine")
                }
                ConversionConsumer::ArraySliceLength
                | ConversionConsumer::ArraySliceStart
                | ConversionConsumer::ArraySliceEnd => {
                    unreachable!("Array slice conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayBufferSliceStart
                | ConversionConsumer::ArrayBufferSliceEnd => {
                    unreachable!("ArrayBuffer slice conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayBufferTransferLength(_) => {
                    unreachable!("ArrayBuffer transfer conversion resumes inside its state machine")
                }
                ConversionConsumer::ArraySpliceStart
                | ConversionConsumer::ArraySpliceLength
                | ConversionConsumer::ArraySpliceDeleteCount => {
                    unreachable!("Array splice conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayRemoveLength => {
                    unreachable!("Array removal conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayInsertLength => {
                    unreachable!("Array insertion conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayReverseLength => {
                    unreachable!("Array reverse conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayFillLength
                | ConversionConsumer::ArrayFillStart
                | ConversionConsumer::ArrayFillEnd => {
                    unreachable!("Array fill conversion resumes inside its state machine")
                }
                ConversionConsumer::ArrayJoinLength
                | ConversionConsumer::ArrayJoinSeparator
                | ConversionConsumer::ArrayJoinElement => {
                    unreachable!("Array join conversion resumes inside its state machine")
                }
                ConversionConsumer::StringSplitReceiver
                | ConversionConsumer::StringSplitLimit
                | ConversionConsumer::StringSplitSeparator => {
                    unreachable!("String split conversion resumes inside its state machine")
                }
                ConversionConsumer::StringReplaceAllFlags
                | ConversionConsumer::StringReplaceAllReceiver
                | ConversionConsumer::StringReplaceAllSearch
                | ConversionConsumer::StringReplaceAllReplacement => {
                    unreachable!("String replaceAll conversion resumes inside its state machine")
                }
                ConversionConsumer::StringPrototypeReceiver
                | ConversionConsumer::StringPrototypeString
                | ConversionConsumer::StringPrototypeFiller
                | ConversionConsumer::StringPrototypeFirstNumber
                | ConversionConsumer::StringPrototypeSecondNumber => {
                    unreachable!("String prototype conversion resumes inside its state machine")
                }
                ConversionConsumer::RegExpTestInput => {
                    unreachable!("RegExp test conversion resumes inside its state machine")
                }
                ConversionConsumer::RegExpSearchInput
                | ConversionConsumer::RegExpReplaceResult
                | ConversionConsumer::RegExpStringIteratorMatch
                | ConversionConsumer::RegExpStringIteratorLastIndex
                | ConversionConsumer::StringSearchReceiver
                | ConversionConsumer::StringSearchPattern => {
                    unreachable!("RegExp search conversion resumes inside its state machine")
                }
                ConversionConsumer::RegExpExecInput => {
                    unreachable!("RegExp exec conversion resumes inside its state machine")
                }
                ConversionConsumer::RegExpLastIndex => {
                    unreachable!("RegExp lastIndex conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayByteOffset
                | ConversionConsumer::TypedArrayLength
                | ConversionConsumer::TypedArrayElement => {
                    unreachable!("TypedArray conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayIndexSet => {
                    unreachable!("TypedArray indexed set resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayAtIndex => {
                    unreachable!("TypedArray at conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayWithIndex
                | ConversionConsumer::TypedArrayWithValue => {
                    unreachable!("TypedArray with conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayIncludesFromIndex => {
                    unreachable!("TypedArray includes conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayFillValue
                | ConversionConsumer::TypedArrayFillStart
                | ConversionConsumer::TypedArrayFillEnd => {
                    unreachable!("TypedArray fill conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayCopyWithinTarget
                | ConversionConsumer::TypedArrayCopyWithinStart
                | ConversionConsumer::TypedArrayCopyWithinEnd => {
                    unreachable!(
                        "TypedArray copyWithin conversion resumes inside its state machine"
                    )
                }
                ConversionConsumer::TypedArraySearchFromIndex => {
                    unreachable!("TypedArray search conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArraySetOffset
                | ConversionConsumer::TypedArraySetLength
                | ConversionConsumer::TypedArraySetElement => {
                    unreachable!("TypedArray set conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArrayJoinSeparator => {
                    unreachable!("TypedArray join conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArraySliceStart
                | ConversionConsumer::TypedArraySliceEnd => {
                    unreachable!("TypedArray slice conversion resumes inside its state machine")
                }
                ConversionConsumer::TypedArraySubarrayStart
                | ConversionConsumer::TypedArraySubarrayEnd => {
                    unreachable!("TypedArray subarray conversion resumes inside its state machine")
                }
                ConversionConsumer::NativeCall(_) | ConversionConsumer::NativeConstruct(_) => {
                    unreachable!("native conversion consumers always carry a native function")
                }
            });
        };
        match native {
            NativeFunction::StringConstructor => {
                let string = self.primitive_string_value(argument)?;
                if matches!(consumer, ConversionConsumer::NativeConstruct(_)) {
                    self.box_string_from_constructor(string, receiver)
                } else {
                    Ok(string)
                }
            }
            NativeFunction::SymbolConstructor => {
                let Some(argument) =
                    argument.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
                else {
                    return self.allocate_symbol(None);
                };
                let description = self.primitive_to_string_value(argument)?;
                self.allocate_symbol(Some(description))
            }
            NativeFunction::SymbolFor => {
                let argument = argument.unwrap_or(Value::from_immediate(Immediate::Undefined));
                let key = self.primitive_to_string_value(argument)?;
                self.symbol_for_string(key)
            }
            NativeFunction::StringIterator => {
                let string = self.primitive_string_value(argument)?;
                self.create_string_iterator(string)
            }
            NativeFunction::StringTrim => {
                let receiver = self.primitive_to_string_value(
                    argument.ok_or(ExecutionError::MissingNativeContinuation)?,
                )?;
                self.string_trim(receiver, true, true)
            }
            NativeFunction::StringTrimStart => {
                let receiver = self.primitive_to_string_value(
                    argument.ok_or(ExecutionError::MissingNativeContinuation)?,
                )?;
                self.string_trim(receiver, true, false)
            }
            NativeFunction::StringTrimEnd => {
                let receiver = self.primitive_to_string_value(
                    argument.ok_or(ExecutionError::MissingNativeContinuation)?,
                )?;
                self.string_trim(receiver, false, true)
            }
            NativeFunction::NumberConstructor => {
                let argument = argument.unwrap_or(Value::from_i32(0));
                let number = if self.is_bigint_value(argument) {
                    self.bigint_to_number_value(argument)?
                } else {
                    self.convert_to_number(argument)?
                };
                if matches!(consumer, ConversionConsumer::NativeConstruct(_)) {
                    self.box_number_from_constructor(number, receiver)
                } else {
                    Ok(number)
                }
            }
            NativeFunction::BigIntConstructor => self.bigint_constructor_primitive(
                argument.unwrap_or(Value::from_immediate(Immediate::Undefined)),
            ),
            NativeFunction::BigIntToString => self.bigint_to_string(receiver, argument),
            NativeFunction::BigIntAsIntN | NativeFunction::BigIntAsUintN => self
                .finish_bigint_as_n(
                    argument.unwrap_or(Value::from_immediate(Immediate::Undefined)),
                    receiver,
                    native == NativeFunction::BigIntAsIntN,
                ),
            NativeFunction::NumberToExponential => self.number_to_exponential(receiver, argument),
            NativeFunction::NumberToFixed => self.number_to_fixed(receiver, argument),
            NativeFunction::NumberToPrecision => self.number_to_precision(receiver, argument),
            NativeFunction::NumberToString => self.number_to_string(receiver, argument),
            NativeFunction::GlobalIsFinite
            | NativeFunction::GlobalIsNaN
            | NativeFunction::GlobalParseFloat
            | NativeFunction::GlobalParseInt => self.global_number_primitive_value(
                native
                    .global_number_function()
                    .expect("global numeric native has metadata"),
                argument.unwrap_or(Value::from_immediate(Immediate::Undefined)),
            ),
            NativeFunction::GlobalDecodeUri
            | NativeFunction::GlobalDecodeUriComponent
            | NativeFunction::GlobalEncodeUri
            | NativeFunction::GlobalEncodeUriComponent => self.global_uri_primitive_value(
                native
                    .global_uri_function()
                    .expect("global URI native has metadata"),
                argument.unwrap_or(Value::from_immediate(Immediate::Undefined)),
            ),
            NativeFunction::StringToLowerCase
            | NativeFunction::StringToUpperCase
            | NativeFunction::StringToLocaleLowerCase
            | NativeFunction::StringToLocaleUpperCase => self.string_case_primitive_value(
                argument.ok_or(ExecutionError::MissingNativeContinuation)?,
                matches!(
                    native,
                    NativeFunction::StringToUpperCase | NativeFunction::StringToLocaleUpperCase
                ),
            ),
            NativeFunction::DateParse => self.date_parse_primitive_value(
                argument.ok_or(ExecutionError::MissingNativeContinuation)?,
            ),
            _ => unreachable!("only conversion consumers create this continuation"),
        }
    }

    /// Appends the currently supported ECMAScript primitive string conversion without heap allocation.
    pub(crate) fn append_primitive_string_units(
        &mut self,
        value: Value,
        output: &mut Vec<u16>,
    ) -> Result<(), ExecutionError> {
        if let Some(immediate) = value.as_immediate() {
            let bytes = match immediate {
                Immediate::True => b"true".as_slice(),
                Immediate::False => b"false".as_slice(),
                Immediate::Undefined => b"undefined".as_slice(),
                Immediate::Null => b"null".as_slice(),
                Immediate::Hole | Immediate::Uninitialized => {
                    return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
                }
            };
            output
                .try_reserve(bytes.len())
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            output.extend(bytes.iter().map(|&byte| u16::from(byte)));
            return Ok(());
        }
        if let Some(number) = numeric_value(value) {
            let mut buffer = ryu_js::Buffer::new();
            let printed = if number == 0.0 {
                "0"
            } else {
                buffer.format(number)
            };
            output
                .try_reserve(printed.len())
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            output.extend(printed.bytes().map(u16::from));
            return Ok(());
        }
        if self.is_bigint_value(value) {
            let decimal = self.bigint_decimal_bytes(value)?;
            output
                .try_reserve(decimal.len())
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            output.extend(decimal.into_iter().map(u16::from));
            return Ok(());
        }
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedPrimitiveStringConversion(value))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedPrimitiveStringConversion(value))?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                match string.as_view() {
                    JsStringView::Latin1(bytes) => {
                        output
                            .try_reserve(bytes.len())
                            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                        output.extend(bytes.iter().map(|&byte| u16::from(byte)));
                    }
                    JsStringView::Utf16(units) => {
                        output
                            .try_reserve(units.len())
                            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                        output.extend_from_slice(units);
                    }
                }
                Ok(())
            })
        })
    }

    /// Appends the canonical `Symbol(description)` form without allocating a temporary JS string.
    fn append_symbol_string_units(
        &mut self,
        value: Value,
        output: &mut Vec<u16>,
    ) -> Result<(), ExecutionError> {
        let raw = value
            .as_heap_ref()
            .expect("symbol values always carry a logical heap reference");
        let symbol = self
            .heap
            .checked_reference(raw, self.types.symbol)
            .map_err(ExecutionError::HeapReference)?;
        let description = self.heap.with_running_scope(|scope| {
            let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(symbol, self.types.symbol)
                    .map(|symbol| symbol.description)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        output
            .try_reserve(8)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        output.extend(b"Symbol(".iter().map(|&byte| u16::from(byte)));
        if let Some(description) = description {
            self.append_primitive_string_units(description, output)?;
        }
        output.push(u16::from(b')'));
        Ok(())
    }

    /// Computes the exact code-unit count used by primitive string conversion without allocating.
    pub(crate) fn primitive_string_unit_length(
        &mut self,
        value: Value,
    ) -> Result<usize, ExecutionError> {
        if let Some(immediate) = value.as_immediate() {
            return match immediate {
                Immediate::True => Ok(4),
                Immediate::False => Ok(5),
                Immediate::Undefined => Ok(9),
                Immediate::Null => Ok(4),
                Immediate::Hole | Immediate::Uninitialized => {
                    Err(ExecutionError::UnsupportedPrimitiveStringConversion(value))
                }
            };
        }
        if let Some(number) = numeric_value(value) {
            let mut buffer = ryu_js::Buffer::new();
            return Ok(if number == 0.0 {
                1
            } else {
                buffer.format(number).len()
            });
        }
        if self.is_bigint_value(value) {
            return self
                .bigint_decimal_bytes(value)
                .map(|decimal| decimal.len());
        }
        if self.is_string_value(value) {
            return self.string_value_length(value);
        }
        Err(ExecutionError::UnsupportedPrimitiveStringConversion(value))
    }

    /// Implements primitive Add after both operands have completed default-hint ToPrimitive.
    pub(crate) fn add_primitive_values(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<Value, ExecutionError> {
        if self.is_string_value(left) || self.is_string_value(right) {
            if self.is_symbol_value(left) || self.is_symbol_value(right) {
                return Err(ExecutionError::NotObject(if self.is_symbol_value(left) {
                    left
                } else {
                    right
                }));
            }
            let capacity = self
                .primitive_string_unit_length(left)?
                .checked_add(self.primitive_string_unit_length(right)?)
                .ok_or(ExecutionError::StringBufferAllocationFailed)?;
            let mut units = Vec::new();
            units
                .try_reserve_exact(capacity)
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            self.append_primitive_string_units(left, &mut units)?;
            self.append_primitive_string_units(right, &mut units)?;
            debug_assert_eq!(units.len(), capacity);
            let string = JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?;
            return self.allocate_runtime_string(string);
        }
        self.numeric_primitive_binary_operation(Opcode::Add, left, right)
    }

    /// Compares two primitive strings by exact ECMAScript UTF-16 code-unit ordering.
    pub(crate) fn compare_string_values(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<core::cmp::Ordering, ExecutionError> {
        let left = left
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(left))?;
        let right = right
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(right))?;
        let left = self
            .heap
            .checked_reference(left, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedStringValue(Value::from_heap_ref(left)))?;
        let right = self
            .heap
            .checked_reference(right, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedStringValue(Value::from_heap_ref(right)))?;
        self.heap.with_running_scope(|scope| {
            let left = scope.root(left).map_err(ExecutionError::Root)?;
            let right = scope.root(right).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let left = no_gc
                    .borrow(left, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let right = no_gc
                    .borrow(right, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(left.as_view().cmp(&right.as_view()))
            })
        })
    }

    /// Implements relational comparison after both operands have completed number-hint ToPrimitive.
    pub(crate) fn relational_primitive_values(
        &mut self,
        opcode: Opcode,
        left: Value,
        right: Value,
    ) -> Result<Value, ExecutionError> {
        if self.is_string_value(left) && self.is_string_value(right) {
            let ordering = self.compare_string_values(left, right)?;
            let result = match opcode {
                Opcode::LessThan => ordering.is_lt(),
                Opcode::GreaterThan => ordering.is_gt(),
                Opcode::LessEqual => ordering.is_le(),
                Opcode::GreaterEqual => ordering.is_ge(),
                _ => unreachable!("relational consumer received a non-relational opcode"),
            };
            return Ok(Value::from_immediate(if result {
                Immediate::True
            } else {
                Immediate::False
            }));
        }
        let left = self.convert_to_number(left)?;
        let right = self.convert_to_number(right)?;
        Ok(numeric_relational(opcode, left, right))
    }

    /// Publishes one runtime-created string through the ordinary managed external allocation path.
    pub(crate) fn allocate_runtime_string(
        &mut self,
        string: JsString,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        let value = self
            .heap
            .try_allocate_external_with_gc(
                self.types.string,
                0,
                string,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.realm
            .retain_construction_value(Value::from_heap_ref(value.raw()))
    }

    /// Allocates a String while updating one caller-owned edge across a moving collection.
    pub(crate) fn allocate_runtime_string_retaining(
        &mut self,
        string: JsString,
        retained: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        let mut roots = RuntimeStringAllocationRoots {
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
            retained,
        };
        let value = self
            .heap
            .try_allocate_external_with_gc(
                self.types.string,
                0,
                string,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok((Value::from_heap_ref(value.raw()), roots.retained))
    }

    /// Converts the primitive values represented by the current numeric VM subset.
    #[inline(always)]
    pub(crate) fn convert_to_number(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(value);
        }
        match value.as_immediate() {
            Some(Immediate::True) => Ok(Value::from_i32(1)),
            Some(Immediate::False | Immediate::Null) => Ok(Value::from_i32(0)),
            Some(Immediate::Undefined) => Ok(Value::from_f64(f64::NAN)),
            Some(Immediate::Hole | Immediate::Uninitialized) => {
                Err(ExecutionError::UnsupportedNumberConversion(value))
            }
            None => {
                let raw = value
                    .as_heap_ref()
                    .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
                if self.heap.checked_reference(raw, self.types.symbol).is_ok() {
                    return Err(ExecutionError::NotObject(value));
                }
                let Ok(reference) = self.heap.checked_reference(raw, self.types.string) else {
                    if self.is_object_value(value) {
                        let value_of = self.intern_intrinsic_name(b"valueOf")?;
                        let to_string = self.intern_intrinsic_name(b"toString")?;
                        let value_of = self.get_data_property(value, value_of)?;
                        let to_string = self.get_data_property(value, to_string)?;
                        let has_callable = [value_of, to_string]
                            .into_iter()
                            .flatten()
                            .any(|method| self.resolve_function_object(method).is_ok());
                        if !has_callable {
                            return Err(ExecutionError::NotObject(value));
                        }
                    }
                    return Err(ExecutionError::UnsupportedNumberConversion(value));
                };
                let units = self.heap.with_running_scope(|scope| {
                    let root = scope.root(reference).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let string = no_gc
                            .borrow(root, self.types.string)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        let units = match string.as_view() {
                            JsStringView::Latin1(bytes) => {
                                bytes.iter().map(|&byte| u16::from(byte)).collect()
                            }
                            JsStringView::Utf16(units) => units.to_vec(),
                        };
                        Ok::<_, ExecutionError>(units)
                    })
                })?;
                Ok(Value::from_f64(parse_number_code_units(&units)))
            }
        }
    }

    #[inline(always)]
    pub(crate) fn typeof_value(&mut self, value: Value) -> Result<Value, ExecutionError> {
        let strings = self.realm.typeof_strings;
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(strings.number);
        }
        if self.is_bigint_value(value) {
            return Ok(strings.bigint);
        }
        if let Some(immediate) = value.as_immediate() {
            return match immediate {
                Immediate::Undefined => Ok(strings.undefined),
                Immediate::Null => Ok(strings.object),
                Immediate::False | Immediate::True => Ok(strings.boolean),
                Immediate::Hole | Immediate::Uninitialized => {
                    Err(ExecutionError::UnsupportedTypeof(value))
                }
            };
        }
        let mut value = value;
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedTypeof(value))?;
        if self.heap.checked_reference(raw, self.types.string).is_ok() {
            return Ok(strings.string);
        }
        if self.heap.checked_reference(raw, self.types.symbol).is_ok() {
            return Ok(strings.symbol);
        }
        while let Some(raw) = value.as_heap_ref()
            && let Ok(proxy) = self.heap.checked_reference(raw, self.types.proxy_object)
        {
            value = self.heap.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference(proxy, self.types.proxy_object)
                    .map(|proxy| proxy.target)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
        }
        if let Some(raw) = value.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.function)
                .is_ok()
        {
            return Ok(strings.function);
        }
        if self.is_object_value(value) {
            return Ok(strings.object);
        }
        Err(ExecutionError::UnsupportedTypeof(value))
    }

    /// Implements SameValue, including NaN equality and signed-zero distinction.
    pub(crate) fn same_value(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        if let (Some(left), Some(right)) = (numeric_value(left), numeric_value(right)) {
            if left.is_nan() && right.is_nan() {
                return Ok(true);
            }
            if left == 0.0 && right == 0.0 {
                return Ok(left.is_sign_negative() == right.is_sign_negative());
            }
            return Ok(left == right);
        }
        self.strict_equal_values(left, right)
    }

    /// Implements SameValueZero for Array and keyed collections without allocating.
    #[inline(always)]
    pub(crate) fn same_value_zero(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<bool, ExecutionError> {
        if let (Some(left), Some(right)) = (numeric_value(left), numeric_value(right)) {
            return Ok((left.is_nan() && right.is_nan()) || left == right);
        }
        self.strict_equal_values(left, right)
    }

    /// Applies strict equality without allocating while preserving numeric and string semantics.
    pub(crate) fn strict_equal_values(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<bool, ExecutionError> {
        match (numeric_value(left), numeric_value(right)) {
            (Some(left), Some(right)) => return Ok(left == right),
            (Some(_), None) | (None, Some(_)) => return Ok(false),
            (None, None) => {}
        }
        let left_bigint = self.is_bigint_value(left);
        let right_bigint = self.is_bigint_value(right);
        if left_bigint || right_bigint {
            return if left_bigint && right_bigint {
                self.bigint_equal(left, right)
            } else {
                Ok(false)
            };
        }
        if left == right {
            return Ok(true);
        }
        let (Some(left), Some(right)) = (left.as_heap_ref(), right.as_heap_ref()) else {
            return Ok(false);
        };
        let Ok(left) = self.heap.checked_reference(left, self.types.string) else {
            return Ok(false);
        };
        let Ok(right) = self.heap.checked_reference(right, self.types.string) else {
            return Ok(false);
        };
        self.heap.with_running_scope(|scope| {
            let left = scope.root(left).map_err(ExecutionError::Root)?;
            let right = scope.root(right).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let left = no_gc
                    .borrow(left, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let right = no_gc
                    .borrow(right, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(left == right)
            })
        })
    }

    #[inline(always)]
    pub(crate) fn is_truthy_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if let Some(raw) = value.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(string, self.types.string)
                        .map(|string| !string.is_empty())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            });
        }
        Ok(is_non_string_truthy(value))
    }
}

/// Resolves strict equality without heap access, deferring distinct heap strings to the slow path.
#[inline(always)]
pub(crate) fn strict_equal_hot(left: Value, right: Value) -> Option<bool> {
    match (numeric_value(left), numeric_value(right)) {
        (Some(left), Some(right)) => return Some(left == right),
        (Some(_), None) | (None, Some(_)) => return Some(false),
        (None, None) => {}
    }
    match (left.as_small_bigint(), right.as_small_bigint()) {
        (Some(left), Some(right)) => return Some(left == right),
        (Some(_), None) | (None, Some(_)) => return Some(false),
        (None, None) => {}
    }
    if left == right {
        return Some(true);
    }
    if left.as_heap_ref().is_some() && right.as_heap_ref().is_some() {
        return None;
    }
    Some(false)
}

#[inline(always)]
pub(crate) fn boolean_value(value: bool) -> Value {
    Value::from_immediate(if value {
        Immediate::True
    } else {
        Immediate::False
    })
}

#[inline(always)]
pub(crate) fn is_non_string_truthy(value: Value) -> bool {
    if let Some(integer) = value.as_i32() {
        return integer != 0;
    }
    if let Some(number) = value.as_f64() {
        return number != 0.0 && !number.is_nan();
    }
    if let Some(bigint) = value.as_small_bigint() {
        return bigint != 0;
    }
    !matches!(
        value.as_immediate(),
        Some(Immediate::Undefined | Immediate::Null | Immediate::False)
    )
}

#[inline(always)]
pub(crate) fn is_nullish(value: Value) -> bool {
    matches!(
        value.as_immediate(),
        Some(Immediate::Undefined | Immediate::Null)
    )
}
