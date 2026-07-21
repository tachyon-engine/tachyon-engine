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

impl Trace for ErrorAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.message.trace(tracer);
    }
}

impl Isolate {
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
        let mut roots = ErrorAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
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
        Ok(error)
    }

    /// Implements the non-accessor Error.prototype.toString path with exact UTF-16 assembly.
    pub(crate) fn error_to_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let name_atom = self.intern_intrinsic_name(b"name")?;
        let message_atom = self.message_atom()?;
        let name = match self.get_data_property(receiver, name_atom)? {
            None => None,
            Some(value) if value.as_immediate() == Some(Immediate::Undefined) => None,
            Some(value) => Some(value),
        };
        let message = self
            .get_data_property(receiver, message_atom)?
            .filter(|value| value.as_immediate() != Some(Immediate::Undefined));
        let name_length = name.map_or(Ok(5), |value| self.error_string_length(value))?;
        let message_length = message.map_or(Ok(0), |value| self.error_string_length(value))?;
        let separator = usize::from(name_length != 0 && message_length != 0) * 2;
        let capacity = name_length
            .checked_add(separator)
            .and_then(|length| length.checked_add(message_length))
            .ok_or(ExecutionError::InvalidStringLength)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        match name {
            Some(value) => self.append_error_string_value(value, &mut output)?,
            None => output.extend(b"Error".iter().map(|&byte| u16::from(byte))),
        }
        if separator != 0 {
            output.extend([u16::from(b':'), u16::from(b' ')]);
        }
        if let Some(value) = message {
            self.append_error_string_value(value, &mut output)?;
        }
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
