//! Observable Get/Call boundaries and IteratorClose precedence for lazy helpers.

use super::super::*;
use super::lazy::IteratorHelperEffect;

impl Isolate {
    /// Completes flatMap and closes its outer iterator after an inner/setup abrupt.
    pub(super) fn begin_iterator_helper_outer_error(
        &mut self,
        site: NativeContinuationSite,
        helper: Value,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        self.complete_iterator_helper(helper)?;
        let original = match error {
            ExecutionError::HostThrown(value) => value,
            error => {
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                self.write(site.caller_base, site.destination, helper)?;
                let original = self.create_native_error(kind, None)?;
                let helper = self.read(site.caller_base, site.destination)?;
                return self.begin_iterator_helper_throw_close(
                    site,
                    IteratorHelperStage::AbruptCloseReturnGet,
                    helper,
                    original,
                );
            }
        };
        self.begin_iterator_helper_throw_close(
            site,
            IteratorHelperStage::AbruptCloseReturnGet,
            helper,
            original,
        )
    }

    /// Converts an immediate native callback error into the helper's explicit close policy.
    fn handle_iterator_helper_call_error(
        &mut self,
        continuation: NativeContinuation,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let NativeContinuationKind::IteratorHelper(stage) = continuation.kind() else {
            return Err(error);
        };
        match stage {
            IteratorHelperStage::CallbackCall => {
                let site = continuation.site();
                let helper = continuation.first();
                self.complete_iterator_helper(helper)?;
                self.write(site.caller_base, site.destination, helper)?;
                let thrown = match error {
                    ExecutionError::HostThrown(value) => value,
                    error => {
                        let Some(kind) = execution_error_kind(&error) else {
                            return Err(error);
                        };
                        self.create_native_error(kind, None)?
                    }
                };
                let helper = self.read(site.caller_base, site.destination)?;
                self.begin_iterator_helper_throw_close(
                    site,
                    IteratorHelperStage::AbruptCloseReturnGet,
                    helper,
                    thrown,
                )
            }
            IteratorHelperStage::NextCall => {
                self.complete_iterator_helper(continuation.first())?;
                Err(error)
            }
            IteratorHelperStage::CreateCloseReturnCall
            | IteratorHelperStage::AbruptCloseReturnCall => {
                Err(ExecutionError::HostThrown(continuation.second()))
            }
            _ => Err(error),
        }
    }

    /// Starts throw-completion IteratorClose for creation validation or callback abrupt.
    pub(super) fn begin_iterator_helper_throw_close(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        owner: Value,
        original: Value,
    ) -> Result<(), ExecutionError> {
        let iterator = if stage == IteratorHelperStage::CreateCloseReturnGet {
            owner
        } else {
            self.iterator_helper_value_snapshot(owner)?.outer_iterator
        };
        let return_key = self.intern_intrinsic_name(b"return")?;
        self.dispatch_iterator_helper_get(site, stage, owner, original, iterator, return_key.into())
    }

    /// Applies throw-completion precedence after observable return lookup.
    pub(super) fn resume_iterator_helper_throw_close(
        &mut self,
        continuation: NativeContinuation,
        stage: IteratorHelperStage,
        method: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        if is_nullish(method) || !self.is_callable_value(method)? {
            return Err(ExecutionError::HostThrown(continuation.second()));
        }
        let iterator = if stage == IteratorHelperStage::CreateCloseReturnGet {
            continuation.first()
        } else {
            self.iterator_helper_value_snapshot(continuation.first())?
                .outer_iterator
        };
        let call_stage = if stage == IteratorHelperStage::CreateCloseReturnGet {
            IteratorHelperStage::CreateCloseReturnCall
        } else {
            IteratorHelperStage::AbruptCloseReturnCall
        };
        self.call_iterator_helper(
            site,
            call_stage,
            continuation.first(),
            method,
            iterator,
            continuation.second(),
            &[],
        )
    }

    /// Applies normal-completion IteratorClose and validates its return object.
    pub(super) fn resume_iterator_helper_normal_close_get(
        &mut self,
        continuation: NativeContinuation,
        method: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        if is_nullish(method) {
            return self.finish_iterator_helper_done(site);
        }
        self.resolve_function_object(method)?;
        let iterator = self
            .iterator_helper_value_snapshot(continuation.first())?
            .outer_iterator;
        self.call_iterator_helper(
            site,
            IteratorHelperStage::NormalCloseReturnCall,
            continuation.first(),
            method,
            iterator,
            Value::from_immediate(Immediate::Undefined),
            &[],
        )
    }

    /// Performs a resumable property Get with the helper operation as its parent.
    pub(super) fn dispatch_iterator_helper_get(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        first: Value,
        second: Value,
        target: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let effect =
            self.dispatch_iterator_helper_get_effect(site, stage, first, second, target, key)?;
        let Some((continuation, stage, value)) = effect.resumed() else {
            return Ok(());
        };
        self.resume_iterator_helper(continuation, stage, value)
    }

    /// Performs one property Get and bounces synchronous completion back to the driver loop.
    pub(super) fn dispatch_iterator_helper_get_effect(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        first: Value,
        second: Value,
        target: Value,
        key: PropertyKey,
    ) -> Result<IteratorHelperEffect, ExecutionError> {
        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_helper(
                site, stage, first, second,
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_aware_property_read(site, target, target, key);
        if let Err(error) = outcome {
            let continuation = self.pop_native_continuation()?;
            if matches!(
                stage,
                IteratorHelperStage::NextCall
                    | IteratorHelperStage::DoneGet
                    | IteratorHelperStage::ValueGet
            ) {
                self.complete_iterator_helper(first)?;
            }
            if matches!(
                stage,
                IteratorHelperStage::CreateCloseReturnGet
                    | IteratorHelperStage::AbruptCloseReturnGet
            ) {
                return Err(ExecutionError::HostThrown(continuation.second()));
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth || self.fiber.completions.len() <= depth {
            return Ok(IteratorHelperEffect::Settled);
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        Ok(IteratorHelperEffect::Resume(continuation, returned))
    }

    /// Calls one cached iterator method or callback through an immutable exact argument prefix.
    #[allow(
        clippy::too_many_arguments,
        reason = "the typed call boundary keeps stage, roots, receiver, and exact arguments explicit"
    )]
    pub(super) fn call_iterator_helper(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        owner: Value,
        callee: Value,
        receiver: Value,
        retained: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        let effect = self.call_iterator_helper_effect(
            site, stage, owner, callee, receiver, retained, arguments,
        )?;
        let Some((continuation, stage, value)) = effect.resumed() else {
            return Ok(());
        };
        self.resume_iterator_helper(continuation, stage, value)
    }

    /// Calls one method and bounces synchronous completion back to the shared driver loop.
    #[allow(
        clippy::too_many_arguments,
        reason = "the typed call boundary keeps stage, roots, receiver, and exact arguments explicit"
    )]
    pub(super) fn call_iterator_helper_effect(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        owner: Value,
        callee: Value,
        receiver: Value,
        retained: Value,
        arguments: &[Value],
    ) -> Result<IteratorHelperEffect, ExecutionError> {
        self.resolve_function_object(callee)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_helper(
                site, stage, owner, retained,
            ))
            .map_err(Self::completion_stack_error)?;
        let prefix_result = if arguments.is_empty() {
            None
        } else {
            let mut copied = Vec::new();
            copied
                .try_reserve_exact(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
            copied.extend_from_slice(arguments);
            Some(self.create_apply_argument_prefix(callee, receiver, copied)?)
        };
        let prefix = prefix_result;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: prefix,
            argument_prefix_offset: 0,
            argument_prefix_count: arguments.len() as u32,
            argument_count: arguments.len() as u32,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            let continuation = self.pop_native_continuation()?;
            self.handle_iterator_helper_call_error(continuation, error)?;
            return Ok(IteratorHelperEffect::Settled);
        }
        let parent_is_active = self.fiber.completions.last_native().is_some_and(|parent| {
            matches!(parent.kind(), NativeContinuationKind::IteratorHelper(parent_stage) if parent_stage == stage)
                && parent.first() == owner
        });
        if !parent_is_active {
            return Ok(IteratorHelperEffect::Settled);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Iterator Helper callback publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(IteratorHelperEffect::Settled);
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        Ok(IteratorHelperEffect::Resume(continuation, returned))
    }

    /// Returns the helper continuation itself or the parent below a generic getter callback.
    pub(super) fn iterator_helper_effective_continuation(
        &self,
        continuation: NativeContinuation,
    ) -> Option<NativeContinuation> {
        if matches!(
            continuation.kind(),
            NativeContinuationKind::IteratorHelper(_)
        ) {
            Some(continuation)
        } else {
            self.fiber
                .completions
                .last_native()
                .filter(|parent| matches!(parent.kind(), NativeContinuationKind::IteratorHelper(_)))
        }
    }
}
