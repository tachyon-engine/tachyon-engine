//! Resumable `RegExp.prototype.flags` observable property reads.

use super::*;

const FLAGS_RECEIVER: usize = 1;
const FLAGS_MASK: usize = 0;
const FLAG_NAMES: [&[u8]; 8] = [
    b"hasIndices",
    b"global",
    b"ignoreCase",
    b"multiline",
    b"dotAll",
    b"unicode",
    b"unicodeSets",
    b"sticky",
];
const FLAG_UNITS: [u8; 8] = *b"dgimsuvy";

impl Isolate {
    /// Starts the ordered Get sequence without reading any flag before state publication.
    pub(crate) fn begin_regexp_flags(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        if !self.is_object_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let state = self.allocate_regexp_exec_state(receiver, Value::from_i32(0), 0)?;
        self.write(
            native_site.caller_base,
            native_site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.dispatch_regexp_flags_read(native_site, state, 0)
    }

    /// Records one ToBoolean result and continues with the next specification-ordered flag.
    pub(crate) fn resume_regexp_flags(
        &mut self,
        continuation: NativeContinuation,
        index: u8,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.native_call_state_reference(continuation.first())?;
        let mut mask = self.native_call_state_snapshot(state)?.values[FLAGS_MASK]
            .as_i32()
            .ok_or(ExecutionError::InvalidRegExpFlags)? as u8;
        if self.is_truthy_value(value)? {
            mask |= 1_u8 << index;
            self.update_regexp_exec_state_value(state, FLAGS_MASK, Value::from_i32(mask.into()))?;
        }
        let next = index.saturating_add(1);
        if usize::from(next) == FLAG_NAMES.len() {
            self.finish_regexp_flags(continuation.site(), mask)
        } else {
            self.dispatch_regexp_flags_read(continuation.site(), state, next)
        }
    }

    /// Performs one Proxy/accessor-aware Get while retaining receiver and accumulated mask.
    fn dispatch_regexp_flags_read(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        index: u8,
    ) -> Result<(), ExecutionError> {
        let receiver = self.native_call_state_snapshot(state)?.values[FLAGS_RECEIVER];
        let name = FLAG_NAMES
            .get(usize::from(index))
            .ok_or(ExecutionError::InvalidRegExpFlags)?;
        let key = self.intern_intrinsic_name(name)?;
        let continuation = NativeContinuation::regexp_flags(
            site,
            index,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        if let Err(error) =
            self.dispatch_proxy_aware_property_read(site, receiver, receiver, key.into())
        {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_regexp_flags(continuation, index, value)
    }

    /// Materializes the at-most-eight ASCII flags without a heap-growing temporary buffer.
    fn finish_regexp_flags(
        &mut self,
        site: NativeContinuationSite,
        mask: u8,
    ) -> Result<(), ExecutionError> {
        let mut bytes = [0_u8; FLAG_UNITS.len()];
        let mut length = 0;
        for (index, flag) in FLAG_UNITS.into_iter().enumerate() {
            if mask & (1_u8 << index) != 0 {
                bytes[length] = flag;
                length += 1;
            }
        }
        let result = self.allocate_runtime_string(
            JsString::try_from_latin1(&bytes[..length]).map_err(ExecutionError::ConstantString)?,
        )?;
        self.write(site.caller_base, site.destination, result)
    }
}
