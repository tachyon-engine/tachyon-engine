//! ECMAScript BigInt wrapper, prototype, and fixed-width operations.

use super::super::*;

impl Isolate {
    /// Extracts `[[BigIntData]]` from a primitive or genuine BigInt wrapper.
    pub(crate) fn this_bigint_value(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_bigint_value(value) {
            return Ok(value);
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let bigint = self
            .heap
            .checked_reference(raw, self.types.bigint_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(bigint, self.types.bigint_object)
                    .map(|bigint| bigint.bigint_data)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Formats a branded BigInt after observable radix conversion has completed.
    pub(crate) fn bigint_to_string(
        &mut self,
        receiver: Value,
        radix: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let bigint = self.this_bigint_value(receiver)?;
        let radix = match radix {
            None => 10,
            Some(value) if value.as_immediate() == Some(Immediate::Undefined) => 10,
            Some(value) => {
                if self.is_bigint_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                let value = self.convert_to_number(value)?;
                let number = numeric_value(value)
                    .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
                let integer = if number.is_nan() { 0.0 } else { number.trunc() };
                if !(2.0..=36.0).contains(&integer) {
                    return Err(ExecutionError::InvalidNumberRadix(value));
                }
                integer as u8
            }
        };
        let bytes = self.bigint_radix_bytes(bigint, radix)?;
        self.allocate_runtime_string(
            JsString::try_from_latin1(&bytes).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Boxes one BigInt primitive through the Realm-local intrinsic prototype.
    pub(crate) fn box_bigint(&mut self, bigint: Value) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .bigint_prototype
            .expect("BigInt prototype initializes before primitive boxing");
        self.allocate_bigint_object(bigint, prototype, AllocationSpace::Young)
    }

    /// Completes asIntN/asUintN after both observable conversions are primitive.
    pub(crate) fn finish_bigint_as_n(
        &mut self,
        bits: Value,
        bigint: Value,
        signed: bool,
    ) -> Result<Value, ExecutionError> {
        let bits = self.ecma_to_index(bits)?;
        let bigint = self.primitive_to_bigint(bigint)?;
        self.bigint_as_n(bits, bigint, signed)
    }
}
