//! Branded ECMAScript Error payloads and allocation.

use super::*;

/// An Error instance has an unforgeable VM brand and shared ordinary property storage.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ErrorObject {
    pub(crate) kind: NativeErrorKind,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for ErrorObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
    }
}

struct ErrorAllocationRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    message: Option<Value>,
}

struct ErrorConstructorRoots<'a> {
    vm: VmRoots<'a>,
    error: Value,
    options: Value,
}

struct ErrorToStringRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
}

impl Trace for ErrorToStringRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
    }
}

impl Trace for ErrorConstructorRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.error.trace(tracer);
        self.options.trace(tracer);
    }
}

const ERROR_STATE_ERROR: usize = 0;
const ERROR_STATE_OPTIONS: usize = 1;
const ERROR_STATE_MESSAGE: usize = 2;
const ERROR_STRING_RECEIVER: usize = 0;
const ERROR_STRING_NAME: usize = 1;
const ERROR_STRING_MESSAGE: usize = 2;

impl Trace for ErrorAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.message.trace(tracer);
    }
}

impl Isolate {
    /// Begins the fully observable Error.prototype.toString Get/ToString sequence.
    pub(crate) fn begin_error_to_string(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.this_value) {
            return Err(ExecutionError::NotObject(site.this_value));
        }
        let state = self.allocate_error_to_string_state(site.this_value)?;
        self.dispatch_error_to_string_get(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            state,
            ErrorToStringStage::NameValue,
        )
    }

    /// Resumes the name or message property after an accessor/Proxy callback.
    pub(crate) fn resume_error_to_string(
        &mut self,
        continuation: NativeContinuation,
        stage: ErrorToStringStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ErrorToStringStage::NameValue => {
                if value.as_immediate() == Some(Immediate::Undefined) {
                    let name = self.with_error_state_root(continuation, |isolate| {
                        isolate.allocate_runtime_string(
                            JsString::try_from_latin1(b"Error")
                                .map_err(ExecutionError::PropertyKeyString)?,
                        )
                    })?;
                    return self.finish_error_to_string_name(continuation.site(), state, name);
                }
                self.convert_error_to_string_part(
                    continuation.site(),
                    state,
                    value,
                    ConversionConsumer::ErrorToStringName,
                )
            }
            ErrorToStringStage::MessageValue => {
                if value.as_immediate() == Some(Immediate::Undefined) {
                    let message = self.with_error_state_root(continuation, |isolate| {
                        isolate.allocate_runtime_string(
                            JsString::try_from_latin1(b"")
                                .map_err(ExecutionError::PropertyKeyString)?,
                        )
                    })?;
                    return self.finish_error_to_string_message(
                        continuation.site(),
                        state,
                        message,
                    );
                }
                self.convert_error_to_string_part(
                    continuation.site(),
                    state,
                    value,
                    ConversionConsumer::ErrorToStringMessage,
                )
            }
        }
    }

    /// Stores the converted name before starting the ordered message Get.
    pub(crate) fn finish_error_to_string_name(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        name: Value,
    ) -> Result<(), ExecutionError> {
        self.set_error_state_value(state, ERROR_STRING_NAME, name)?;
        self.dispatch_error_to_string_get(site, state, ErrorToStringStage::MessageValue)
    }

    /// Stores the converted message and assembles the exact final UTF-16 result.
    pub(crate) fn finish_error_to_string_message(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        message: Value,
    ) -> Result<(), ExecutionError> {
        self.set_error_state_value(state, ERROR_STRING_MESSAGE, message)?;
        let snapshot = self.native_call_state_snapshot(state)?;
        let result = self.with_error_state_root(
            NativeContinuation::error_to_string(
                site,
                ErrorToStringStage::MessageValue,
                Value::from_heap_ref(state.raw()),
            ),
            |isolate| {
                isolate.assemble_error_strings(
                    snapshot.values[ERROR_STRING_NAME],
                    snapshot.values[ERROR_STRING_MESSAGE],
                )
            },
        )?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Starts message conversion and the ordered resumable InstallErrorCause operation.
    pub(crate) fn begin_error_constructor(
        &mut self,
        site: &CallSite,
        kind: NativeErrorKind,
    ) -> Result<(), ExecutionError> {
        let message = self.call_argument(site, 0)?;
        let options = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let intrinsic_prototype = self
            .realm
            .error_intrinsics
            .get(kind)
            .prototype
            .expect("native Error prototypes initialize before execution");
        let prototype = if site.new_target.as_immediate() == Some(Immediate::Undefined) {
            intrinsic_prototype
        } else {
            let prototype_atom = self.prototype_atom()?;
            self.constructor_prototype_value(site.new_target, prototype_atom)?
                .filter(|value| self.is_object_value(*value))
                .unwrap_or(intrinsic_prototype)
        };
        let error = self.allocate_native_error_with_prototype(kind, None, prototype)?;
        let state = self.allocate_error_constructor_state(error, options)?;
        let state_value = Value::from_heap_ref(state.raw());
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let Some(message) =
            message.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return self.continue_error_cause(continuation_site, state);
        };
        if self.is_object_value(message) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ErrorConstructorMessage,
                site.caller_base,
                site.destination,
                state_value,
                message,
                site.call_site,
            );
        }
        let message = self.with_error_state_root(
            NativeContinuation::error_constructor(
                continuation_site,
                ErrorConstructorStage::HasCause,
                state_value,
            ),
            |isolate| isolate.error_message_string(message),
        )?;
        self.finish_error_message(continuation_site, state, message)
    }

    /// Continues Error construction after one nested HasProperty/Get operation completes.
    pub(crate) fn resume_error_constructor(
        &mut self,
        continuation: NativeContinuation,
        stage: ErrorConstructorStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        match stage {
            ErrorConstructorStage::HasCause => {
                if !self.is_truthy_value(value)? {
                    return self.finish_error_constructor(continuation.site(), state);
                }
                self.dispatch_error_cause_get(continuation.site(), state)
            }
            ErrorConstructorStage::CauseValue => {
                let snapshot = self.native_call_state_snapshot(state)?;
                self.with_error_state_root(continuation, |isolate| {
                    let cause = isolate.intern_intrinsic_name(b"cause")?;
                    isolate.define_data_property(
                        snapshot.values[ERROR_STATE_ERROR],
                        cause,
                        DataPropertyDescriptor {
                            value: Some(value),
                            writable: Some(true),
                            enumerable: Some(false),
                            configurable: Some(true),
                        },
                    )
                })?;
                self.finish_error_constructor(continuation.site(), state)
            }
        }
    }

    /// Installs one already-converted message and proceeds to the options object.
    pub(crate) fn finish_error_message(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        message: Value,
    ) -> Result<(), ExecutionError> {
        self.set_error_state_value(state, ERROR_STATE_MESSAGE, message)?;
        let snapshot = self.native_call_state_snapshot(state)?;
        self.with_error_state_root(
            NativeContinuation::error_constructor(
                site,
                ErrorConstructorStage::HasCause,
                Value::from_heap_ref(state.raw()),
            ),
            |isolate| {
                isolate.define_error_message(
                    snapshot.values[ERROR_STATE_ERROR],
                    snapshot.values[ERROR_STATE_MESSAGE],
                )
            },
        )?;
        self.continue_error_cause(site, state)
    }

    /// Allocates an exact two-value construction state before any user callback can run.
    fn allocate_error_constructor_state(
        &mut self,
        error: Value,
        options: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = ErrorConstructorRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            error,
            options,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                NativeCallState {
                    values: [
                        roots.error,
                        roots.options,
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                    ],
                    count: 2,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates three fixed traced slots for receiver, converted name, and converted message.
    fn allocate_error_to_string_state(
        &mut self,
        receiver: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = ErrorToStringRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            receiver,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                NativeCallState {
                    values: [
                        roots.receiver,
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                    ],
                    count: 3,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Converts an object through string-hint ToPrimitive or a primitive through ToString.
    fn convert_error_to_string_part(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
        consumer: ConversionConsumer,
    ) -> Result<(), ExecutionError> {
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
        let stage = match consumer {
            ConversionConsumer::ErrorToStringName => ErrorToStringStage::NameValue,
            ConversionConsumer::ErrorToStringMessage => ErrorToStringStage::MessageValue,
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        let value = self.with_error_state_root(
            NativeContinuation::error_to_string(site, stage, Value::from_heap_ref(state.raw())),
            |isolate| isolate.error_message_string(value),
        )?;
        match consumer {
            ConversionConsumer::ErrorToStringName => {
                self.finish_error_to_string_name(site, state, value)
            }
            ConversionConsumer::ErrorToStringMessage => {
                self.finish_error_to_string_message(site, state, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Reads name or message through the shared Proxy/accessor Get continuation path.
    fn dispatch_error_to_string_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: ErrorToStringStage,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[ERROR_STRING_RECEIVER];
        let key = match stage {
            ErrorToStringStage::NameValue => self.name_atom()?,
            ErrorToStringStage::MessageValue => self.message_atom()?,
        };
        self.dispatch_error_nested_operation(
            NativeContinuation::error_to_string(site, stage, Value::from_heap_ref(state.raw())),
            |isolate| {
                isolate
                    .dispatch_proxy_aware_property_read(site, receiver, receiver, key.into())
                    .map(|_| ())
            },
        )
    }

    /// Updates one traced state slot with its old-to-young write barrier.
    fn set_error_state_value(
        &mut self,
        state: GcRef<NativeCallState>,
        index: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[index] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Temporarily republishes a popped Error continuation while synchronous work may allocate.
    pub(crate) fn with_error_state_root<T>(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<T, ExecutionError>,
    ) -> Result<T, ExecutionError> {
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let result = operation(self);
        let pop = self.pop_native_continuation();
        match (result, pop) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(_)) => Ok(value),
        }
    }

    /// Rejects Symbol and converts every other primitive with ordinary ToString semantics.
    pub(crate) fn error_message_string(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_value(Some(value))
    }

    /// Performs HasProperty(options, "cause") with an Error parent continuation.
    fn continue_error_cause(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let options = self.native_call_state_snapshot(state)?.values[ERROR_STATE_OPTIONS];
        if !self.is_object_value(options) {
            return self.finish_error_constructor(site, state);
        }
        let continuation = NativeContinuation::error_constructor(
            site,
            ErrorConstructorStage::HasCause,
            Value::from_heap_ref(state.raw()),
        );
        let key = self.with_error_state_root(continuation, |isolate| {
            let cause = isolate.intern_intrinsic_name(b"cause")?;
            isolate.atom_string_value(cause)
        })?;
        self.dispatch_error_nested_operation(continuation, |isolate| {
            isolate
                .dispatch_has_property(site, options, key)
                .map(|_| ())
        })
    }

    /// Performs Get(options, "cause") while preserving the options receiver through Proxy/accessor paths.
    fn dispatch_error_cause_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let options = self.native_call_state_snapshot(state)?.values[ERROR_STATE_OPTIONS];
        let continuation = NativeContinuation::error_constructor(
            site,
            ErrorConstructorStage::CauseValue,
            Value::from_heap_ref(state.raw()),
        );
        let cause = self.with_error_state_root(continuation, |isolate| {
            isolate.intern_intrinsic_name(b"cause")
        })?;
        self.dispatch_error_nested_operation(continuation, |isolate| {
            isolate
                .dispatch_proxy_aware_property_read(site, options, options, cause.into())
                .map(|_| ())
        })
    }

    /// Runs one nested resumable operation and drains its parent immediately when it stays synchronous.
    fn dispatch_error_nested_operation(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<(), ExecutionError>,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = operation(self) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let site = continuation.site();
        let value = self.read(site.caller_base, site.destination)?;
        match continuation.kind() {
            NativeContinuationKind::ErrorConstructor(stage) => {
                self.resume_error_constructor(continuation, stage, value)
            }
            NativeContinuationKind::ErrorToString(stage) => {
                self.resume_error_to_string(continuation, stage, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Writes the final branded Error after all observable constructor steps have completed.
    fn finish_error_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let error = self.native_call_state_snapshot(state)?.values[ERROR_STATE_ERROR];
        self.write(site.caller_base, site.destination, error)
    }

    /// Allocates a branded Error and installs the optional non-enumerable message property.
    pub(crate) fn create_native_error(
        &mut self,
        kind: NativeErrorKind,
        message: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .error_intrinsics
            .get(kind)
            .prototype
            .expect("native Error prototypes initialize before execution");
        self.allocate_native_error_with_prototype(kind, message, prototype)
    }

    /// Allocates a native Error using the intrinsic prototype belonging to another Realm.
    pub(crate) fn create_native_error_in_realm(
        &mut self,
        kind: NativeErrorKind,
        message: Option<Value>,
        realm: RealmId,
    ) -> Result<Value, ExecutionError> {
        let prototype = if realm == self.active_realm {
            self.realm.error_intrinsics.get(kind).prototype
        } else {
            self.inactive_realms
                .iter()
                .find(|(id, _)| *id == realm)
                .and_then(|(_, realm)| realm.error_intrinsics.get(kind).prototype)
        }
        .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?;
        self.allocate_native_error_with_prototype(kind, message, prototype)
    }

    fn allocate_native_error_with_prototype(
        &mut self,
        kind: NativeErrorKind,
        message: Option<Value>,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut roots = ErrorAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
            message,
        };
        let error = self
            .heap
            .try_allocate_with_gc(
                self.types.error_object,
                0,
                0,
                ErrorObject {
                    kind,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let error = Value::from_heap_ref(error.raw());
        let Some(message) = roots
            .message
            .filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(error);
        };
        self.define_error_message(error, message)?;
        Ok(error)
    }

    /// Installs the non-enumerable message after verifying the converted value is a String.
    fn define_error_message(&mut self, error: Value, message: Value) -> Result<(), ExecutionError> {
        let raw = message
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedErrorMessage(message))?;
        self.heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedErrorMessage(message))?;
        let message_atom = self.message_atom()?;
        self.define_data_property(
            error,
            message_atom,
            DataPropertyDescriptor {
                value: Some(message),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        Ok(())
    }

    /// Assembles two converted String values with the spec's empty-component rules.
    fn assemble_error_strings(
        &mut self,
        name: Value,
        message: Value,
    ) -> Result<Value, ExecutionError> {
        let name_length = self.error_string_length(name)?;
        let message_length = self.error_string_length(message)?;
        let separator = usize::from(name_length != 0 && message_length != 0) * 2;
        let capacity = name_length
            .checked_add(separator)
            .and_then(|length| length.checked_add(message_length))
            .ok_or(ExecutionError::InvalidStringLength)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        self.append_error_string_value(name, &mut output)?;
        if separator != 0 {
            output.extend([u16::from(b':'), u16::from(b' ')]);
        }
        self.append_error_string_value(message, &mut output)?;
        self.allocate_runtime_string(
            JsString::try_from_utf16(&output).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Computes the exact capacity for the primitive-only Error string conversion path.
    fn error_string_length(&mut self, value: Value) -> Result<usize, ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_unit_length(value)
    }

    /// Converts a primitive property while keeping Symbol's implicit ToString rejection explicit.
    fn append_error_string_value(
        &mut self,
        value: Value,
        output: &mut Vec<u16>,
    ) -> Result<(), ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.append_primitive_string_units(value, output)
    }
}
