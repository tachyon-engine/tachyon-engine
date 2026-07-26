//! Resumable `%TypedArray.prototype%.at` relative-index access.

use super::*;

impl Isolate {
    /// Validates the receiver before index conversion and resumes object indices without recursion.
    pub(crate) fn begin_typed_array_at(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        self.typed_array_at_snapshot(receiver)?;
        let index = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(index) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::TypedArrayAtIndex,
                site.caller_base,
                site.destination,
                receiver,
                index,
                site.call_site,
            );
        }
        self.finish_typed_array_at(continuation_site, receiver, index)
    }

    /// Revalidates the fixed view after an observable ToPrimitive callback.
    pub(crate) fn resume_typed_array_at_conversion(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.finish_typed_array_at(site, receiver, value)
    }

    /// Applies ToIntegerOrInfinity, relative indexing, and one current-backing element read.
    fn finish_typed_array_at(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let relative_index = if number.is_nan() || number == 0.0 {
            0.0
        } else {
            number.trunc()
        };
        let snapshot = self.typed_array_at_snapshot(receiver)?;
        let index = if relative_index >= 0.0 {
            relative_index
        } else {
            snapshot.length as f64 + relative_index
        };
        let result = if !(0.0..snapshot.length as f64).contains(&index) {
            Value::from_immediate(Immediate::Undefined)
        } else {
            self.typed_array_read_element(snapshot, index as usize)?
        };
        self.write(site.caller_base, site.destination, result)
    }

    /// Produces a fixed-view snapshot only after proving the current backing is attached.
    fn typed_array_at_snapshot(
        &mut self,
        receiver: Value,
    ) -> Result<TypedArraySnapshot, ExecutionError> {
        let snapshot = self.typed_array_snapshot(receiver)?;
        self.typed_array_backing(snapshot.buffer)?;
        Ok(snapshot)
    }
}
