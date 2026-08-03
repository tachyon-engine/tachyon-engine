//! Resumable `@@toPrimitive` calls and their separately rooted hint argument window.

use super::{super::*, ConversionCallbackResult};

impl Isolate {
    /// Calls `@@toPrimitive` with one hint while rooting a getter-produced callable separately.
    pub(super) fn call_exotic_conversion_callback(
        &mut self,
        continuation: ConversionContinuation,
        callee: Value,
        hint: Value,
    ) -> Result<ConversionCallbackResult, ExecutionError> {
        let argument_base = continuation
            .site
            .caller_base
            .checked_add(continuation.site.destination)
            .ok_or(ExecutionError::RegisterWindowTooLarge(1))?;
        self.push_native_conversion(continuation)?;
        if let Err(error) =
            self.push_conversion_call_root(continuation.site, continuation.object, callee)
        {
            self.pop_native_conversion()?;
            return Err(error);
        }
        if let Err(error) = self.write(
            continuation.site.caller_base,
            continuation.site.destination,
            hint,
        ) {
            self.pop_conversion_call_root()?;
            self.pop_native_conversion()?;
            return Err(error);
        }
        let root_kind = NativeContinuationKind::ConversionCallRoot;
        let frame_depth = self.fiber.frames.len();
        let call_result = self.call(CallSite {
            caller_base: continuation.site.caller_base,
            destination: continuation.site.destination,
            callee,
            argument_base,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: continuation.object,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: continuation.site.call_site,
        });
        if let Err(error) = call_result {
            if self
                .fiber
                .completions
                .last_native_matches(root_kind, continuation.site)
            {
                self.pop_conversion_call_root()?;
                self.pop_native_conversion()?;
            }
            return Err(error);
        }
        if !self
            .fiber
            .completions
            .last_native_matches(root_kind, continuation.site)
        {
            return Ok(ConversionCallbackResult::Suspended);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a suspended exotic conversion publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(ConversionCallbackResult::Suspended);
        }
        self.pop_conversion_call_root()?;
        let continuation = self.pop_native_conversion()?;
        self.read(continuation.site.caller_base, continuation.site.destination)
            .map(ConversionCallbackResult::Returned)
    }

    /// Pushes the child call root without changing the parent conversion payload.
    fn push_conversion_call_root(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        callee: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::conversion_call_root(
                site, receiver, callee,
            ))
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })
    }

    /// Pops only the child call root, preserving its conversion parent below it.
    fn pop_conversion_call_root(&mut self) -> Result<(), ExecutionError> {
        let continuation = self.pop_native_continuation()?;
        if continuation.kind() != NativeContinuationKind::ConversionCallRoot {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        Ok(())
    }
}
