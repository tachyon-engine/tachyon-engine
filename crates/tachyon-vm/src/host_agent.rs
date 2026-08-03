//! Opaque host-agent calls used by embedding adapters such as the Test262 runner.

use crate::{
    AgentBroadcast, AgentBroadcastValue, CallSite, ExecutionError, HostAgentFunction, Immediate,
    Isolate, JsString, NativeFunction, OrdinaryObject, ShapeId, Value, numeric_value,
};

impl Isolate {
    /// Installs the raw agent capability object; adapters may wrap these entries in JavaScript.
    pub(crate) fn install_host_agent_hooks(
        &mut self,
        hooks: Value,
        function_prototype: Value,
    ) -> Result<(), ExecutionError> {
        if self.host_providers.agent_host_mut().is_none() {
            return Ok(());
        }
        let agent = self.create_ordinary_object()?;
        for function in HostAgentFunction::ALL {
            let callable = self.allocate_native_function(
                NativeFunction::HostAgent(function),
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            let atom = self.intern_intrinsic_name(function.name().as_bytes())?;
            self.set_own_data_property(agent, atom, callable)?;
        }
        let agent_atom = self.intern_intrinsic_name(b"agent")?;
        self.set_own_data_property(hooks, agent_atom, agent)
    }

    /// Executes one raw agent call after the adapter's JavaScript wrapper performs conversions.
    pub(crate) fn call_host_agent(
        &mut self,
        site: &CallSite,
        function: HostAgentFunction,
    ) -> Result<(), ExecutionError> {
        match function {
            HostAgentFunction::Start => self.call_host_agent_start(site),
            HostAgentFunction::Broadcast => self.call_host_agent_broadcast(site),
            HostAgentFunction::ReceiveBroadcast => self.call_host_agent_receive(site),
            HostAgentFunction::Report => self.call_host_agent_report(site),
            HostAgentFunction::GetReport => self.call_host_agent_get_report(site),
            HostAgentFunction::Sleep => self.call_host_agent_sleep(site),
            HostAgentFunction::MonotonicNow => self.call_host_agent_monotonic_now(site),
            HostAgentFunction::Leaving => self.call_host_agent_leaving(site),
        }
    }

    fn call_host_agent_start(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let source = self.call_argument_or_undefined(site, 0)?;
        let source = self.string_value_to_utf16(source)?.into_boxed_slice();
        self.agent_host()?
            .start(source)
            .map_err(ExecutionError::AgentHostProvider)?;
        self.write_host_agent_undefined(site)
    }

    /// Converts the wrapper-normalized scalar into an isolate-neutral owned broadcast message.
    fn call_host_agent_broadcast(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let buffer = self.call_argument_or_undefined(site, 0)?;
        let handle = self.export_shared_array_buffer(buffer)?;
        let scalar = self.call_argument_or_undefined(site, 1)?;
        let value = if scalar.as_immediate() == Some(Immediate::Undefined) {
            AgentBroadcastValue::Undefined
        } else if self.is_bigint_value(scalar) {
            let string = self.bigint_to_string(scalar, None)?;
            AgentBroadcastValue::BigInt(self.string_value_to_utf16(string)?.into_boxed_slice())
        } else {
            let number =
                numeric_value(scalar).ok_or(ExecutionError::UnsupportedNumberConversion(scalar))?;
            AgentBroadcastValue::Int32(number as i32)
        };
        self.agent_host()?
            .broadcast(AgentBroadcast {
                buffer: handle,
                value,
            })
            .map_err(ExecutionError::AgentHostProvider)?;
        self.write_host_agent_undefined(site)
    }

    /// Rebuilds one broadcast packet while rooting every intermediate across forced collections.
    fn call_host_agent_receive(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let message = self
            .agent_host()?
            .receive_broadcast()
            .map_err(ExecutionError::AgentHostProvider)?;
        let packet = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, packet)?;
        let buffer_atom = self.intern_intrinsic_name(b"buffer")?;
        let value_atom = self.intern_intrinsic_name(b"value")?;
        let buffer = self.import_shared_array_buffer(message.buffer)?;
        self.set_own_data_property(packet, buffer_atom, buffer)?;
        let value = match message.value {
            AgentBroadcastValue::Undefined => Value::from_immediate(Immediate::Undefined),
            AgentBroadcastValue::Int32(value) => Value::from_i32(value),
            AgentBroadcastValue::BigInt(units) => {
                let string = self.allocate_runtime_string(
                    JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
                )?;
                self.primitive_to_bigint(string)?
            }
        };
        self.set_own_data_property(packet, value_atom, value)?;
        Ok(())
    }

    fn call_host_agent_report(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let message = self.call_argument_or_undefined(site, 0)?;
        let message = self.string_value_to_utf16(message)?.into_boxed_slice();
        self.agent_host()?
            .report(message)
            .map_err(ExecutionError::AgentHostProvider)?;
        self.write_host_agent_undefined(site)
    }

    fn call_host_agent_get_report(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let report = self
            .agent_host()?
            .get_report()
            .map_err(ExecutionError::AgentHostProvider)?;
        let value = match report {
            Some(units) => self.allocate_runtime_string(
                JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
            )?,
            None => Value::from_immediate(Immediate::Null),
        };
        self.write(site.caller_base, site.destination, value)
    }

    fn call_host_agent_sleep(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let value = self.call_argument_or_undefined(site, 0)?;
        let milliseconds =
            numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        self.agent_host()?
            .sleep(milliseconds)
            .map_err(ExecutionError::AgentHostProvider)?;
        self.write_host_agent_undefined(site)
    }

    fn call_host_agent_monotonic_now(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let value = self
            .agent_host()?
            .monotonic_now()
            .map_err(ExecutionError::AgentHostProvider)?;
        self.write(site.caller_base, site.destination, Value::from_f64(value))
    }

    fn call_host_agent_leaving(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.agent_host()?
            .leaving()
            .map_err(ExecutionError::AgentHostProvider)?;
        self.write_host_agent_undefined(site)
    }

    #[inline]
    fn call_argument_or_undefined(
        &mut self,
        site: &CallSite,
        index: u32,
    ) -> Result<Value, ExecutionError> {
        Ok(self
            .call_argument(site, index)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined)))
    }

    #[inline]
    fn agent_host(
        &mut self,
    ) -> Result<&mut (dyn crate::AgentHostProvider + 'static), ExecutionError> {
        self.host_providers
            .agent_host_mut()
            .ok_or(ExecutionError::MissingAgentHostProvider)
    }

    #[inline]
    fn write_host_agent_undefined(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(Immediate::Undefined),
        )
    }
}
