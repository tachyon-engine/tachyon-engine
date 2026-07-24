//! Array constructor and prototype slow paths.

use super::super::*;

impl Isolate {
    /// Creates the current Realm's Array argument list for a Proxy apply/construct trap.
    pub(crate) fn create_array_argument_list_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let mut site = *site;
        site.new_target = Value::from_immediate(Immediate::Undefined);
        self.create_array_from_site(&site)
    }

    /// Creates an Array-shaped ordinary object from one native call/construct argument window.
    pub(crate) fn create_array_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let count = usize::try_from(site.argument_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            );
        }
        let array_length = if arguments.len() == 1 {
            numeric_value(arguments[0])
                .map(|length| {
                    if !length.is_finite()
                        || length < 0.0
                        || length.fract() != 0.0
                        || length > f64::from(u32::MAX)
                    {
                        return Err(ExecutionError::InvalidArrayLength);
                    }
                    Ok(safe_integer_value(length as u64))
                })
                .transpose()?
        } else {
            None
        };
        let default_prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before Array construction");
        let prototype = if self.is_object_value(site.new_target) {
            let prototype_atom = self.prototype_atom()?;
            self.constructor_prototype_value(site.new_target, prototype_atom)?
                .filter(|value| self.is_object_value(*value))
                .or_else(|| {
                    self.realm_for_callable(site.new_target)
                        .ok()
                        .and_then(|realm| {
                            self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::Array)
                        })
                })
                .unwrap_or(default_prototype)
        } else {
            default_prototype
        };
        let array = self.create_array_object_with_prototype(prototype)?;
        self.write(site.caller_base, site.destination, array)?;
        let length_atom = self.intern_intrinsic_name(b"length")?;
        if let Some(length) = array_length {
            self.set_own_data_property(array, length_atom, length)?;
            return Ok(array);
        }
        for (index, value) in arguments.into_iter().enumerate() {
            let index = i32::try_from(index)
                .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
            let key = self.property_key_atom(Value::from_i32(index))?;
            self.set_own_data_property(array, key, value)?;
        }
        let length = Value::from_i32(
            i32::try_from(count)
                .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?,
        );
        self.set_own_data_property(array, length_atom, length)?;
        Ok(array)
    }

    /// Implements IsArray with a direct payload fast path and recursive Proxy target traversal.
    pub(crate) fn is_array_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        let mut current = value;
        loop {
            if current
                .as_heap_ref()
                .is_some_and(|raw| self.heap.checked_reference(raw, self.types.array).is_ok())
            {
                return Ok(true);
            }
            if !self.is_proxy_value(current) {
                return Ok(false);
            }
            let proxy = self.proxy_snapshot(current)?;
            if proxy.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            current = proxy.target;
        }
    }

    /// Applies the currently supported ToLength boundary to one object length property.
    fn length_of_array_like(&mut self, receiver: Value) -> Result<u64, ExecutionError> {
        if !self.is_object_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let length_atom = self.length_atom()?;
        let value = self
            .get_data_property(receiver, length_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let number = self.convert_to_number(value)?;
        let number =
            numeric_value(number).ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
            return Ok(MAX_SAFE_INTEGER);
        }
        Ok(number.floor() as u64)
    }

    /// Implements `Array.prototype.at` for the supported generic array-like receiver.
    pub(crate) fn array_at(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        if length == 0 {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        let index_value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let number = numeric_value(self.convert_to_number(index_value)?).unwrap_or(f64::NAN);
        if number.is_nan() {
            return self.array_element_or_undefined(site.this_value, 0);
        }
        let index = if number < 0.0 {
            length as f64 + number.ceil()
        } else {
            number.floor()
        };
        if !(0.0..(length as f64)).contains(&index) {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        self.array_element_or_undefined(site.this_value, index as u64)
    }

    fn array_element_or_undefined(
        &mut self,
        receiver: Value,
        index: u64,
    ) -> Result<Value, ExecutionError> {
        let key = self.safe_integer_property_atom(index)?;
        Ok(self
            .get_data_property(receiver, key)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined)))
    }
}
