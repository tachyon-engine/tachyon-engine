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

    /// Implements Array.prototype.push through the generic array-like Set contract.
    pub(crate) fn array_push(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let argument_count = u64::from(site.argument_count);
        let new_length = length
            .checked_add(argument_count)
            .filter(|length| *length <= MAX_SAFE_INTEGER)
            .ok_or(ExecutionError::ArrayLengthOverflow)?;
        for index in 0..site.argument_count {
            let value = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            let key = self.safe_integer_property_atom(
                length
                    .checked_add(u64::from(index))
                    .ok_or(ExecutionError::ArrayLengthOverflow)?,
            )?;
            self.set_own_data_property(site.this_value, key, value)?;
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(site.this_value, length_atom, safe_integer_value(new_length))?;
        Ok(safe_integer_value(new_length))
    }

    pub(crate) fn array_join(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let separator = self.call_argument(site, 0)?;
        self.join_array_like(site.this_value, separator)
    }

    /// Joins one generic array-like receiver while retaining primitive conversion order.
    fn join_array_like(
        &mut self,
        receiver: Value,
        separator: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(receiver)?;
        let mut separator_units = Vec::new();
        if separator.is_none_or(|value| value.as_immediate() == Some(Immediate::Undefined)) {
            separator_units
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            separator_units.push(u16::from(b','));
        } else if let Some(separator) = separator {
            self.append_primitive_string_units(separator, &mut separator_units)?;
        }
        let per_element =
            tuning::arrays::JOIN_INITIAL_UNITS_PER_ELEMENT.saturating_add(separator_units.len());
        let estimated = usize::try_from(length)
            .unwrap_or(usize::MAX)
            .saturating_mul(per_element)
            .min(tuning::arrays::JOIN_MAX_INITIAL_UNITS);
        let mut output = Vec::new();
        output
            .try_reserve_exact(estimated)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..length {
            if index != 0 {
                output
                    .try_reserve(separator_units.len())
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                output.extend_from_slice(&separator_units);
            }
            let key = self.safe_integer_property_atom(index)?;
            let value = self
                .get_data_property(receiver, key)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            if value == receiver
                || matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                )
            {
                continue;
            }
            self.append_primitive_string_units(value, &mut output)?;
        }
        let string =
            JsString::try_from_utf16(&output).map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(string)
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

    /// Implements `includes` without allocating an iterator or callback closure.
    pub(crate) fn array_search(
        &mut self,
        site: &CallSite,
        includes: bool,
    ) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        if length == 0 {
            return Ok(if includes {
                Value::from_immediate(Immediate::False)
            } else {
                Value::from_i32(-1)
            });
        }
        let search = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let start_value = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let start_number = numeric_value(self.convert_to_number(start_value)?).unwrap_or(f64::NAN);
        let start = if start_number.is_nan() {
            0
        } else if start_number < 0.0 {
            length.saturating_sub((-start_number).ceil() as u64)
        } else {
            start_number.floor() as u64
        };
        for index in start..length {
            let key = self.safe_integer_property_atom(index)?;
            let Some(value) = self.get_data_property(site.this_value, key)? else {
                if includes && search.as_immediate() == Some(Immediate::Undefined) {
                    return Ok(Value::from_immediate(Immediate::True));
                }
                continue;
            };
            let equal = if includes {
                self.same_value_zero(value, search)?
            } else {
                self.strict_equal_values(value, search)?
            };
            if equal {
                return Ok(if includes {
                    Value::from_immediate(Immediate::True)
                } else {
                    safe_integer_value(index)
                });
            }
        }
        Ok(if includes {
            Value::from_immediate(Immediate::False)
        } else {
            Value::from_i32(-1)
        })
    }

    fn relative_array_index(&mut self, value: Value, length: u64) -> Result<u64, ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?).unwrap_or(f64::NAN);
        if number.is_nan() || number == 0.0 {
            return Ok(0);
        }
        if number.is_sign_negative() {
            return Ok(length.saturating_sub((-number).ceil() as u64));
        }
        Ok(number.floor().min(length as f64) as u64)
    }

    /// Implements `Array.prototype.unshift` with backwards indexed movement and exact length.
    pub(crate) fn array_unshift(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let count = u64::from(site.argument_count);
        let new_length = length
            .checked_add(count)
            .filter(|value| *value <= MAX_SAFE_INTEGER)
            .ok_or(ExecutionError::ArrayLengthOverflow)?;
        for index in (0..length).rev() {
            let source_key = self.safe_integer_property_atom(index)?;
            let target_key = self.safe_integer_property_atom(index + count)?;
            if let Some(value) = self.get_data_property(site.this_value, source_key)? {
                self.set_own_data_property(site.this_value, target_key, value)?;
            } else if !self.delete_own_data_property(site.this_value, target_key)? {
                return Err(ExecutionError::ReadOnlyProperty(site.this_value));
            }
        }
        for index in 0..site.argument_count {
            let value = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            let key = self.safe_integer_property_atom(u64::from(index))?;
            self.set_own_data_property(site.this_value, key, value)?;
        }
        let length_atom = self.length_atom()?;
        self.set_own_data_property(site.this_value, length_atom, safe_integer_value(new_length))?;
        Ok(safe_integer_value(new_length))
    }

    /// Implements `Array.prototype.reverse` by swapping present indexed properties and holes.
    pub(crate) fn array_reverse(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        for lower in 0..(length / 2) {
            let upper = length - lower - 1;
            let lower_key = self.safe_integer_property_atom(lower)?;
            let upper_key = self.safe_integer_property_atom(upper)?;
            let lower_value = self.get_data_property(site.this_value, lower_key)?;
            let upper_value = self.get_data_property(site.this_value, upper_key)?;
            match (lower_value, upper_value) {
                (Some(left), Some(right)) => {
                    self.set_own_data_property(site.this_value, lower_key, right)?;
                    self.set_own_data_property(site.this_value, upper_key, left)?;
                }
                (Some(left), None) => {
                    self.set_own_data_property(site.this_value, upper_key, left)?;
                    if !self.delete_own_data_property(site.this_value, lower_key)? {
                        return Err(ExecutionError::ReadOnlyProperty(site.this_value));
                    }
                }
                (None, Some(right)) => {
                    self.set_own_data_property(site.this_value, lower_key, right)?;
                    if !self.delete_own_data_property(site.this_value, upper_key)? {
                        return Err(ExecutionError::ReadOnlyProperty(site.this_value));
                    }
                }
                (None, None) => {}
            }
        }
        Ok(site.this_value)
    }

    /// Implements `Array.prototype.fill` with ToInteger-relative bounds and hole materialization.
    pub(crate) fn array_fill(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let start_value = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let end_value = self
            .call_argument(site, 2)?
            .unwrap_or(safe_integer_value(length));
        let start = self.relative_array_index(start_value, length)?;
        let end = self.relative_array_index(end_value, length)?;
        for index in start..end {
            let key = self.safe_integer_property_atom(index)?;
            self.set_own_data_property(site.this_value, key, value)?;
        }
        Ok(site.this_value)
    }

    /// Implements `Array.prototype.copyWithin` with overlap-safe direction and hole preservation.
    pub(crate) fn array_copy_within(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let length = self.length_of_array_like(site.this_value)?;
        let target_value = self.call_argument(site, 0)?.unwrap_or(Value::from_i32(0));
        let start_value = self.call_argument(site, 1)?.unwrap_or(Value::from_i32(0));
        let end_value = self
            .call_argument(site, 2)?
            .unwrap_or(safe_integer_value(length));
        let target = self.relative_array_index(target_value, length)?;
        let start = self.relative_array_index(start_value, length)?;
        let end = self.relative_array_index(end_value, length)?;
        let count = end.saturating_sub(start).min(length.saturating_sub(target));
        if target < start || target >= start.saturating_add(count) {
            for offset in 0..count {
                self.copy_within_element(site.this_value, start + offset, target + offset)?;
            }
        } else {
            for offset in (0..count).rev() {
                self.copy_within_element(site.this_value, start + offset, target + offset)?;
            }
        }
        Ok(site.this_value)
    }

    fn copy_within_element(
        &mut self,
        receiver: Value,
        source_index: u64,
        target_index: u64,
    ) -> Result<(), ExecutionError> {
        let source_key = self.safe_integer_property_atom(source_index)?;
        let target_key = self.safe_integer_property_atom(target_index)?;
        if let Some(value) = self.get_data_property(receiver, source_key)? {
            self.set_own_data_property(receiver, target_key, value)?;
        } else if !self.delete_own_data_property(receiver, target_key)? {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        Ok(())
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

    /// Implements Array.prototype.toString as comma-joined primitive elements for this subset.
    pub(crate) fn array_to_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.join_array_like(receiver, None)
    }
}
