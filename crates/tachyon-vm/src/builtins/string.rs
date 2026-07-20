//! Primitive String prototype native methods.

use super::super::*;

impl Isolate {
    /// Implements String.prototype.charAt over the engine's UTF-16 code-unit representation.
    pub(crate) fn string_char_at(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let Some(unit) = self.string_code_unit_at(site)? else {
            return self.allocate_runtime_string(
                JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
            );
        };
        self.allocate_runtime_string(
            JsString::try_from_utf16(&[unit]).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements String.prototype.charCodeAt, returning NaN when the position is outside the input.
    pub(crate) fn string_char_code_at(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        Ok(self.string_code_unit_at(site)?.map_or_else(
            || Value::from_f64(f64::NAN),
            |unit| Value::from_i32(i32::from(unit)),
        ))
    }

    /// Reads one primitive receiver unit after the currently supported ToIntegerOrInfinity conversion.
    fn string_code_unit_at(&mut self, site: &CallSite) -> Result<Option<u16>, ExecutionError> {
        let receiver = site.this_value;
        if !self.is_string_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let position = self
            .call_argument(site, 0)?
            .map(|value| self.convert_to_number(value))
            .transpose()?
            .and_then(numeric_value)
            .unwrap_or(0.0);
        let position = if position.is_nan() || position == 0.0 {
            0.0
        } else {
            position.trunc()
        };
        if !(0.0..=(usize::MAX as f64)).contains(&position) {
            return Ok(None);
        }
        let index = position as usize;
        let raw = receiver.as_heap_ref().expect("primitive String is managed");
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(|string| string.code_unit_at(index))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}
