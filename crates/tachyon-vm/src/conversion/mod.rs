//! Primitive conversion, ToPrimitive continuation, and equality semantics.

mod numeric;

use super::*;
use numeric::parse_number_code_units;

pub(crate) use numeric::{
    numeric_binary, numeric_binary_hot, numeric_binary_operation, numeric_bitwise_not,
    numeric_negate, numeric_relational, numeric_relational_hot, numeric_value, safe_integer_value,
};

enum ConversionCallbackResult {
    Suspended,
    Returned(Value),
}

impl Isolate {
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
    fn primitive_string_value(&mut self, argument: Option<Value>) -> Result<Value, ExecutionError> {
        let Some(argument) = argument else {
            return self.allocate_runtime_string(
                JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
            );
        };
        if self.is_string_value(argument) {
            return Ok(argument);
        }
        let mut units = Vec::new();
        self.append_primitive_string_units(argument, &mut units)?;
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Allocates one unique Symbol primitive while retaining its optional description as a GC edge.
    pub(crate) fn allocate_symbol(
        &mut self,
        description: Option<Value>,
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
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
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
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.next_symbol_serial = next_serial;
        Ok(Value::from_heap_ref(symbol.raw()))
    }

    /// Starts one conversion consumer, suspending only when its argument requires a JS callback.
    pub(crate) fn dispatch_conversion_native(
        &mut self,
        native: NativeFunction,
        site: &CallSite,
        construct: bool,
    ) -> Result<(), ExecutionError> {
        let consumer = if construct {
            ConversionConsumer::NativeConstruct(native)
        } else {
            ConversionConsumer::NativeCall(native)
        };
        let argument = self.call_argument(site, 0)?;
        if let Some(object) = argument
            && self.is_object_value(object)
        {
            let (receiver, stage) = match native {
                NativeFunction::StringConstructor => (
                    Value::from_immediate(Immediate::Undefined),
                    ToPrimitiveStage::ToString,
                ),
                NativeFunction::NumberToExponential
                | NativeFunction::NumberToFixed
                | NativeFunction::NumberToPrecision
                | NativeFunction::NumberToString => (
                    self.this_number_value(site.this_value)?,
                    ToPrimitiveStage::ValueOf,
                ),
                NativeFunction::NumberConstructor => (
                    if construct {
                        site.new_target
                    } else {
                        Value::from_immediate(Immediate::Undefined)
                    },
                    ToPrimitiveStage::ValueOf,
                ),
                _ => unreachable!("only conversion consumers enter this dispatch path"),
            };
            let continuation = ConversionContinuation {
                site: NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                consumer,
                receiver,
                object,
                stage,
                callback_stage: ConversionCallbackStage::MethodCall,
            };
            return self.advance_native_conversion(continuation, None);
        }
        let receiver = if construct {
            site.new_target
        } else {
            site.this_value
        };
        let value = self.finish_conversion_consumer(consumer, receiver, argument)?;
        self.write(site.caller_base, site.destination, value)
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
        debug_assert!(consumer.is_opcode_conversion());
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
                stage: ToPrimitiveStage::ValueOf,
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
                    if continuation.consumer == ConversionConsumer::AddLeft {
                        let left = value;
                        let right = continuation.receiver;
                        if self.is_object_value(right) {
                            continuation.consumer = ConversionConsumer::AddRight;
                            continuation.receiver = left;
                            continuation.object = right;
                            continuation.stage = ToPrimitiveStage::ValueOf;
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
                            continuation.stage = ToPrimitiveStage::ValueOf;
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
                        let left = self.convert_to_number(value)?;
                        let right = continuation.receiver;
                        if self.is_object_value(right) {
                            continuation.consumer = ConversionConsumer::BinaryRight(opcode);
                            continuation.receiver = left;
                            continuation.object = right;
                            continuation.stage = ToPrimitiveStage::ValueOf;
                            continue;
                        }
                        let right = self.convert_to_number(right)?;
                        let result = numeric_binary_operation(opcode, left, right);
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
                        );
                    }
                    if let ConversionConsumer::BinaryRight(opcode) = continuation.consumer {
                        let right = self.convert_to_number(value)?;
                        let result = numeric_binary_operation(opcode, continuation.receiver, right);
                        return self.write(
                            continuation.site.caller_base,
                            continuation.site.destination,
                            result,
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
                let name = match continuation.stage {
                    ToPrimitiveStage::ValueOf => b"valueOf".as_slice(),
                    ToPrimitiveStage::ToString => b"toString".as_slice(),
                };
                let atom = self.intern_intrinsic_name(name)?;
                self.push_native_conversion(continuation)?;
                let property = match self.resolve_property_read(continuation.object, atom.into()) {
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
                        match self.call_conversion_callback(continuation, getter)? {
                            ConversionCallbackResult::Suspended => return Ok(()),
                            ConversionCallbackResult::Returned(value) => {
                                returned = Some(value);
                                continue;
                            }
                        }
                    }
                }
            };
            let Some(method) =
                method.filter(|method| self.resolve_function_object(*method).is_ok())
            else {
                let Some(stage) =
                    next_to_primitive_stage(continuation.consumer, continuation.stage)
                else {
                    return Err(ExecutionError::NotObject(continuation.object));
                };
                continuation.stage = stage;
                continue;
            };
            continuation.callback_stage = ConversionCallbackStage::MethodCall;
            match self.call_conversion_callback(continuation, method)? {
                ConversionCallbackResult::Suspended => return Ok(()),
                ConversionCallbackResult::Returned(value) => returned = Some(value),
            }
        }
    }

    /// Calls one getter or conversion method and reports whether it published a JavaScript frame.
    fn call_conversion_callback(
        &mut self,
        continuation: ConversionContinuation,
        callee: Value,
    ) -> Result<ConversionCallbackResult, ExecutionError> {
        self.push_native_conversion(continuation)?;
        let frame_depth = self.fiber.frames.len();
        let call_result = self.call(CallSite {
            caller_base: continuation.site.caller_base,
            destination: continuation.site.destination,
            callee,
            argument_base: 0,
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
            .push_native(NativeContinuation::Conversion(continuation))
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
        match self.pop_native_continuation()? {
            NativeContinuation::Conversion(continuation) => Ok(continuation),
            NativeContinuation::PropertyGet { .. } | NativeContinuation::PropertySet { .. } => {
                Err(ExecutionError::MissingNativeContinuation)
            }
        }
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
                ConversionConsumer::Negate => numeric_negate(self.convert_to_number(argument)?),
                ConversionConsumer::BitwiseNot => {
                    numeric_bitwise_not(self.convert_to_number(argument)?)
                }
                ConversionConsumer::BinaryLeft(_) | ConversionConsumer::BinaryRight(_) => {
                    unreachable!("binary consumers finish inside the conversion state machine")
                }
                ConversionConsumer::AddLeft | ConversionConsumer::AddRight => {
                    unreachable!("Add consumers finish inside the conversion state machine")
                }
                ConversionConsumer::RelationalLeft(_) | ConversionConsumer::RelationalRight(_) => {
                    unreachable!("relational consumers finish inside the conversion state machine")
                }
                ConversionConsumer::NativeCall(_) | ConversionConsumer::NativeConstruct(_) => {
                    unreachable!("native conversion consumers always carry a native function")
                }
            });
        };
        match native {
            NativeFunction::StringConstructor => self.primitive_string_value(argument),
            NativeFunction::NumberConstructor => {
                let number = self.convert_to_number(argument.unwrap_or(Value::from_i32(0)))?;
                if matches!(consumer, ConversionConsumer::NativeConstruct(_)) {
                    self.box_number_from_constructor(number, receiver)
                } else {
                    Ok(number)
                }
            }
            NativeFunction::NumberToExponential => self.number_to_exponential(receiver, argument),
            NativeFunction::NumberToFixed => self.number_to_fixed(receiver, argument),
            NativeFunction::NumberToPrecision => self.number_to_precision(receiver, argument),
            NativeFunction::NumberToString => self.number_to_string(receiver, argument),
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

    /// Computes the exact code-unit count used by primitive string conversion without allocating.
    fn primitive_string_unit_length(&mut self, value: Value) -> Result<usize, ExecutionError> {
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
        let left = self.convert_to_number(left)?;
        let right = self.convert_to_number(right)?;
        Ok(numeric_binary(Opcode::Add, left, right))
    }

    /// Compares two primitive strings by exact ECMAScript UTF-16 code-unit ordering.
    fn compare_string_values(
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
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
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
        Ok(Value::from_heap_ref(value.raw()))
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
    pub(crate) fn typeof_value(&self, value: Value) -> Result<Value, ExecutionError> {
        let strings = self.realm.typeof_strings;
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(strings.number);
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
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedTypeof(value))?;
        if self.heap.checked_reference(raw, self.types.string).is_ok() {
            return Ok(strings.string);
        }
        if self.heap.checked_reference(raw, self.types.symbol).is_ok() {
            return Ok(strings.symbol);
        }
        if self
            .heap
            .checked_reference(raw, self.types.function)
            .is_ok()
        {
            return Ok(strings.function);
        }
        if self
            .heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
        {
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

    /// Implements the supported primitive subset of Abstract Equality Comparison.
    pub(crate) fn loose_equal_values(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<bool, ExecutionError> {
        if self.strict_equal_values(left, right)? {
            return Ok(true);
        }
        let left_immediate = left.as_immediate();
        let right_immediate = right.as_immediate();
        let left_nullish = matches!(left_immediate, Some(Immediate::Undefined | Immediate::Null));
        let right_nullish = matches!(
            right_immediate,
            Some(Immediate::Undefined | Immediate::Null)
        );
        if left_nullish || right_nullish {
            return Ok(left_nullish && right_nullish);
        }
        let left_number = numeric_value(left);
        let right_number = numeric_value(right);
        if left_number.is_some() && right_number.is_some() {
            return Ok(left_number == right_number);
        }
        let left_boolean = matches!(left_immediate, Some(Immediate::True | Immediate::False));
        let right_boolean = matches!(right_immediate, Some(Immediate::True | Immediate::False));
        if left_boolean || right_boolean || left_number.is_some() || right_number.is_some() {
            let left = self.convert_to_number(left)?;
            let right = self.convert_to_number(right)?;
            return Ok(numeric_value(left) == numeric_value(right));
        }
        Ok(false)
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
