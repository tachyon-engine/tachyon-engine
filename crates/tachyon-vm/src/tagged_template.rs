//! Realm-local tagged-template object creation and code-site caching.

use super::*;

impl Isolate {
    /// Loads one cached template object or constructs both frozen template arrays on a cache miss.
    pub(crate) fn load_template_object(
        &mut self,
        code: CodeId,
        base: u32,
        destination: u32,
        site: u32,
    ) -> Result<(), ExecutionError> {
        let site_index = site as usize;
        if let Some(value) = self
            .loaded_code(code)?
            .constant_values
            .get(site_index)
            .copied()
            .flatten()
        {
            return self.write(base, destination, value);
        }
        let (realm, cooked, raw) = match self
            .loaded_code(code)?
            .module
            .constants()
            .get(site_index)
            .cloned()
        {
            Some(BytecodeConstant::TemplateSite { cooked, raw }) => {
                (self.loaded_code(code)?.realm, cooked, raw)
            }
            _ => return Err(ExecutionError::UnsupportedConstant(site)),
        };
        if cooked.len() != raw.len() {
            return Err(ExecutionError::UnsupportedConstant(site));
        }

        let prototype = self
            .realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::Array)
            .ok_or(ExecutionError::UnsupportedConstant(site))?;
        let raw_key = self.intern_intrinsic_name(b"raw")?;
        let template = self.create_array_object_with_prototype(prototype)?;
        self.write(base, destination, template)?;
        let raw_object = self.create_array_object_with_prototype(prototype)?;

        // Publish the private reachability edge before any element definition can collect.
        self.define_data_property(
            template,
            raw_key,
            template_data_descriptor(raw_object, false),
        )?;
        for (index, (cooked_units, raw_units)) in cooked.iter().zip(raw.iter()).enumerate() {
            let index =
                i32::try_from(index).map_err(|_| ExecutionError::UnsupportedConstant(site))?;
            let key = self.property_key(Value::from_i32(index))?;
            let cooked_value = match cooked_units {
                Some(units) => self.allocate_runtime_string(
                    JsString::try_from_utf16(units).map_err(ExecutionError::ConstantString)?,
                )?,
                None => Value::from_immediate(Immediate::Undefined),
            };
            self.define_data_property(template, key, template_data_descriptor(cooked_value, true))?;
            let raw_value = self.allocate_runtime_string(
                JsString::try_from_utf16(raw_units).map_err(ExecutionError::ConstantString)?,
            )?;
            self.define_data_property(raw_object, key, template_data_descriptor(raw_value, true))?;
        }

        self.object_set_integrity_level(raw_object, true)?;
        self.object_set_integrity_level(template, true)?;
        let template = self.read(base, destination)?;
        let slot = self
            .loaded_code
            .get_mut(code.index())
            .and_then(|loaded| loaded.constant_values.get_mut(site_index))
            .ok_or(ExecutionError::UnsupportedConstant(site))?;
        *slot = Some(template);
        Ok(())
    }
}

#[inline(always)]
const fn template_data_descriptor(value: Value, enumerable: bool) -> PropertyDescriptor {
    PropertyDescriptor::Data(DataPropertyDescriptor {
        value: Some(value),
        writable: Some(false),
        enumerable: Some(enumerable),
        configurable: Some(false),
    })
}
