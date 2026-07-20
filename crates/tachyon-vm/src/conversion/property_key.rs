//! Resumable computed-property key preparation.

use super::super::*;

impl Isolate {
    /// Applies the operation-specific base guard, then prepares a primitive property key in place.
    #[inline(always)]
    pub(crate) fn dispatch_to_property_key(
        &mut self,
        caller_base: u32,
        destination: u32,
        source: u32,
        guard_register: u32,
        require_object: bool,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        let guard = self.read(caller_base, guard_register)?;
        if (require_object && !self.is_object_value(guard))
            || (!require_object && is_nullish(guard))
        {
            return Err(ExecutionError::NotObject(guard));
        }
        let key = self.read(caller_base, source)?;
        if self.is_object_value(key) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ToPropertyKey,
                caller_base,
                destination,
                guard,
                key,
                call_site,
            );
        }
        self.write(caller_base, destination, key)
    }
}
