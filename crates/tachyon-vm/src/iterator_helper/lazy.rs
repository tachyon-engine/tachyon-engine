//! Shared resumable driver for lazy Iterator Helper creation, stepping, and close semantics.

use super::super::*;
use super::{IteratorHelperKind, IteratorHelperState};

enum IteratorHelperEffect {
    Settled,
    Resume(NativeContinuation, Value),
}

impl IteratorHelperEffect {
    #[inline(always)]
    fn resumed(self) -> Option<(NativeContinuation, IteratorHelperStage, Value)> {
        let Self::Resume(continuation, value) = self else {
            return None;
        };
        let NativeContinuationKind::IteratorHelper(stage) = continuation.kind() else {
            unreachable!("Iterator Helper effects retain their typed continuation")
        };
        Some((continuation, stage, value))
    }
}

impl Isolate {
    /// Validates one callback helper before observing and caching its direct next method.
    pub(super) fn begin_iterator_callback_helper(
        &mut self,
        site: &CallSite,
        kind: IteratorHelperKind,
    ) -> Result<(), ExecutionError> {
        debug_assert!(matches!(
            kind,
            IteratorHelperKind::Map | IteratorHelperKind::Filter
        ));
        let iterator = site.this_value;
        if !self.is_object_value(iterator) {
            return Err(ExecutionError::NotObject(iterator));
        }
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let native_site = Self::native_site(site);
        if !self.is_callable_value(callback)? {
            self.write(site.caller_base, site.destination, iterator)?;
            let original = self.create_native_error(NativeErrorKind::Type, None)?;
            let iterator = self.read(site.caller_base, site.destination)?;
            return self.begin_iterator_helper_throw_close(
                native_site,
                IteratorHelperStage::CreateCloseReturnGet,
                iterator,
                original,
            );
        }
        let next = self.intern_intrinsic_name(b"next")?;
        let stage = match kind {
            IteratorHelperKind::Map => IteratorHelperStage::CreateMapNextGet,
            IteratorHelperKind::Filter => IteratorHelperStage::CreateFilterNextGet,
            IteratorHelperKind::Take | IteratorHelperKind::Drop | IteratorHelperKind::FlatMap => {
                unreachable!("only callback helpers use this creation entry point")
            }
        };
        self.dispatch_iterator_helper_get(
            native_site,
            stage,
            iterator,
            callback,
            iterator,
            next.into(),
        )
    }

    /// Converts one take/drop limit through the shared resumable ToNumber machinery.
    pub(super) fn begin_iterator_limit_helper(
        &mut self,
        site: &CallSite,
        kind: IteratorHelperKind,
    ) -> Result<(), ExecutionError> {
        debug_assert!(matches!(
            kind,
            IteratorHelperKind::Take | IteratorHelperKind::Drop
        ));
        let iterator = site.this_value;
        if !self.is_object_value(iterator) {
            return Err(ExecutionError::NotObject(iterator));
        }
        let limit = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let site = Self::native_site(site);
        let stage = if kind == IteratorHelperKind::Take {
            IteratorHelperStage::CreateTakeLimitConversion
        } else {
            IteratorHelperStage::CreateDropLimitConversion
        };
        if !self.is_object_value(limit) {
            return match self.convert_to_number(limit) {
                Ok(number) => self.finish_iterator_limit_conversion(site, stage, iterator, number),
                Err(error) => self.begin_iterator_helper_creation_error(site, iterator, error),
            };
        }

        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_helper(
                site,
                stage,
                iterator,
                Value::from_immediate(Immediate::Undefined),
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.dispatch_object_primitive_conversion(
            ConversionConsumer::ToNumber,
            site.caller_base,
            site.destination,
            Value::from_immediate(Immediate::Undefined),
            limit,
            site.call_site,
        ) {
            return self.handle_iterator_helper_limit_conversion_error(error);
        }
        if self.fiber.frames.len() != frame_depth || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let parent_is_active = self.fiber.completions.last_native().is_some_and(|parent| {
            parent.kind() == NativeContinuationKind::IteratorHelper(stage)
                && parent.first() == iterator
        });
        if !parent_is_active {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let number = self.read(site.caller_base, site.destination)?;
        self.resume_iterator_helper(continuation, stage, number)
    }

    /// Implements `%IteratorHelperPrototype%.next` for the current lazy helper kinds.
    pub(crate) fn begin_iterator_helper_next(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let helper = site.this_value;
        let reference = self.iterator_helper_reference(helper)?;
        let snapshot = self.iterator_helper_snapshot(reference)?;
        match snapshot.state {
            IteratorHelperState::Executing => {
                return Err(ExecutionError::NotObject(helper));
            }
            IteratorHelperState::Completed => {
                let result =
                    self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
                return self.write(site.caller_base, site.destination, result);
            }
            IteratorHelperState::SuspendedYield => {}
            IteratorHelperState::SuspendedStart => {}
        }
        if snapshot.kind == IteratorHelperKind::Take && snapshot.counter_or_limit == 0 {
            self.set_iterator_helper_state(reference, IteratorHelperState::Completed)?;
            let return_key = self.intern_intrinsic_name(b"return")?;
            return self.dispatch_iterator_helper_get(
                Self::native_site(site),
                IteratorHelperStage::NormalCloseReturnGet,
                helper,
                Value::from_immediate(Immediate::Undefined),
                snapshot.outer_iterator,
                return_key.into(),
            );
        }
        self.set_iterator_helper_state(reference, IteratorHelperState::Executing)?;
        if snapshot.kind == IteratorHelperKind::Take && snapshot.counter_or_limit != u64::MAX {
            self.set_iterator_helper_counter(reference, snapshot.counter_or_limit - 1)?;
        }
        self.call_iterator_helper(
            Self::native_site(site),
            IteratorHelperStage::NextCall,
            helper,
            snapshot.outer_next,
            snapshot.outer_iterator,
            Value::from_immediate(Immediate::Undefined),
            &[],
        )
    }

    /// Implements helper return with normal IteratorClose precedence.
    pub(crate) fn begin_iterator_helper_return(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let helper = site.this_value;
        let reference = self.iterator_helper_reference(helper)?;
        let snapshot = self.iterator_helper_snapshot(reference)?;
        if snapshot.state == IteratorHelperState::Executing {
            return Err(ExecutionError::NotObject(helper));
        }
        if snapshot.state == IteratorHelperState::Completed {
            let result =
                self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
            return self.write(site.caller_base, site.destination, result);
        }
        self.set_iterator_helper_state(reference, IteratorHelperState::Completed)?;
        let return_key = self.intern_intrinsic_name(b"return")?;
        self.dispatch_iterator_helper_get(
            Self::native_site(site),
            IteratorHelperStage::NormalCloseReturnGet,
            helper,
            Value::from_immediate(Immediate::Undefined),
            snapshot.outer_iterator,
            return_key.into(),
        )
    }

    /// Resumes one lazy-helper boundary from the interpreter's typed continuation loop.
    pub(crate) fn resume_iterator_helper(
        &mut self,
        mut continuation: NativeContinuation,
        mut stage: IteratorHelperStage,
        mut value: Value,
    ) -> Result<(), ExecutionError> {
        loop {
            let site = continuation.site();
            let effect = match stage {
                IteratorHelperStage::CreateMapNextGet
                | IteratorHelperStage::CreateFilterNextGet => {
                    let kind = if stage == IteratorHelperStage::CreateMapNextGet {
                        IteratorHelperKind::Map
                    } else {
                        IteratorHelperKind::Filter
                    };
                    let helper = self.allocate_iterator_helper(
                        continuation.first(),
                        value,
                        continuation.second(),
                        kind,
                        0,
                    )?;
                    return self.write(site.caller_base, site.destination, helper);
                }
                IteratorHelperStage::CreateTakeLimitConversion
                | IteratorHelperStage::CreateDropLimitConversion => {
                    return self.finish_iterator_limit_conversion(
                        site,
                        stage,
                        continuation.first(),
                        value,
                    );
                }
                IteratorHelperStage::CreateTakeNextGet | IteratorHelperStage::CreateDropNextGet => {
                    let kind = if stage == IteratorHelperStage::CreateTakeNextGet {
                        IteratorHelperKind::Take
                    } else {
                        IteratorHelperKind::Drop
                    };
                    let limit = iterator_helper_limit_from_value(continuation.second())?;
                    let helper = self.allocate_iterator_helper(
                        continuation.first(),
                        value,
                        Value::from_immediate(Immediate::Undefined),
                        kind,
                        limit,
                    )?;
                    return self.write(site.caller_base, site.destination, helper);
                }
                IteratorHelperStage::NextCall => {
                    if !self.is_object_value(value) {
                        self.complete_iterator_helper(continuation.first())?;
                        return Err(ExecutionError::NotObject(value));
                    }
                    let done = self.done_atom()?;
                    self.dispatch_iterator_helper_get_effect(
                        site,
                        IteratorHelperStage::DoneGet,
                        continuation.first(),
                        value,
                        value,
                        done.into(),
                    )?
                }
                IteratorHelperStage::DoneGet => {
                    let helper = continuation.first();
                    if self.is_truthy_value(value)? {
                        self.complete_iterator_helper(helper)?;
                        let result = self.create_iterator_result(
                            Value::from_immediate(Immediate::Undefined),
                            true,
                        )?;
                        return self.write(site.caller_base, site.destination, result);
                    }
                    let snapshot = self.iterator_helper_value_snapshot(helper)?;
                    if snapshot.kind == IteratorHelperKind::Drop && snapshot.counter_or_limit > 0 {
                        if snapshot.counter_or_limit != u64::MAX {
                            self.set_iterator_helper_counter(
                                self.iterator_helper_reference(helper)?,
                                snapshot.counter_or_limit - 1,
                            )?;
                        }
                        self.call_iterator_helper_effect(
                            site,
                            IteratorHelperStage::NextCall,
                            helper,
                            snapshot.outer_next,
                            snapshot.outer_iterator,
                            Value::from_immediate(Immediate::Undefined),
                            &[],
                        )?
                    } else {
                        let result = continuation.second();
                        let value_key = self.value_atom()?;
                        self.dispatch_iterator_helper_get_effect(
                            site,
                            IteratorHelperStage::ValueGet,
                            helper,
                            result,
                            result,
                            value_key.into(),
                        )?
                    }
                }
                IteratorHelperStage::ValueGet => {
                    let helper = continuation.first();
                    let snapshot = self.iterator_helper_value_snapshot(helper)?;
                    if matches!(
                        snapshot.kind,
                        IteratorHelperKind::Take | IteratorHelperKind::Drop
                    ) {
                        self.set_iterator_helper_state(
                            self.iterator_helper_reference(helper)?,
                            IteratorHelperState::SuspendedYield,
                        )?;
                        let result = self.create_iterator_result(value, false)?;
                        return self.write(site.caller_base, site.destination, result);
                    }
                    let counter = safe_integer_value(snapshot.counter_or_limit);
                    let retained = if snapshot.kind == IteratorHelperKind::Filter {
                        value
                    } else {
                        Value::from_immediate(Immediate::Undefined)
                    };
                    self.call_iterator_helper_effect(
                        site,
                        IteratorHelperStage::CallbackCall,
                        helper,
                        snapshot.callback,
                        Value::from_immediate(Immediate::Undefined),
                        retained,
                        &[value, counter],
                    )?
                }
                IteratorHelperStage::CallbackCall => {
                    let helper = continuation.first();
                    let reference = self.iterator_helper_reference(helper)?;
                    let snapshot = self.iterator_helper_snapshot(reference)?;
                    let counter = snapshot
                        .counter_or_limit
                        .checked_add(1)
                        .ok_or(ExecutionError::ArrayLengthOverflow)?;
                    self.set_iterator_helper_counter(reference, counter)?;
                    if snapshot.kind == IteratorHelperKind::Filter
                        && !self.is_truthy_value(value)?
                    {
                        self.call_iterator_helper_effect(
                            site,
                            IteratorHelperStage::NextCall,
                            helper,
                            snapshot.outer_next,
                            snapshot.outer_iterator,
                            Value::from_immediate(Immediate::Undefined),
                            &[],
                        )?
                    } else {
                        self.set_iterator_helper_state(
                            reference,
                            IteratorHelperState::SuspendedYield,
                        )?;
                        let yielded = if snapshot.kind == IteratorHelperKind::Filter {
                            continuation.second()
                        } else {
                            value
                        };
                        let result = self.create_iterator_result(yielded, false)?;
                        return self.write(site.caller_base, site.destination, result);
                    }
                }
                IteratorHelperStage::CreateCloseReturnGet
                | IteratorHelperStage::AbruptCloseReturnGet => {
                    return self.resume_iterator_helper_throw_close(continuation, stage, value);
                }
                IteratorHelperStage::CreateCloseReturnCall
                | IteratorHelperStage::AbruptCloseReturnCall => {
                    return Err(ExecutionError::HostThrown(continuation.second()));
                }
                IteratorHelperStage::NormalCloseReturnGet => {
                    return self.resume_iterator_helper_normal_close_get(continuation, value);
                }
                IteratorHelperStage::NormalCloseReturnCall => {
                    if !self.is_object_value(value) {
                        return Err(ExecutionError::NotObject(value));
                    }
                    return self.finish_iterator_helper_done(site);
                }
            };
            let Some((next, next_stage, next_value)) = effect.resumed() else {
                return Ok(());
            };
            continuation = next;
            stage = next_stage;
            value = next_value;
        }
    }

    /// Handles a thrown JS value at a helper callback/getter boundary.
    pub(crate) fn handle_iterator_helper_thrown(
        &mut self,
        continuation: NativeContinuation,
        thrown: Value,
    ) -> Result<Option<Option<RunOutcome>>, ExecutionError> {
        let Some(parent) = self.iterator_helper_effective_continuation(continuation) else {
            return Ok(None);
        };
        let NativeContinuationKind::IteratorHelper(stage) = parent.kind() else {
            return Ok(None);
        };
        let site = parent.site();
        match stage {
            IteratorHelperStage::CallbackCall => {
                let helper = parent.first();
                self.complete_iterator_helper(helper)?;
                self.begin_iterator_helper_throw_close(
                    site,
                    IteratorHelperStage::AbruptCloseReturnGet,
                    helper,
                    thrown,
                )?;
                Ok(Some(None))
            }
            IteratorHelperStage::CreateCloseReturnGet
            | IteratorHelperStage::CreateCloseReturnCall
            | IteratorHelperStage::AbruptCloseReturnGet
            | IteratorHelperStage::AbruptCloseReturnCall => {
                self.throw_value(parent.second(), site.call_site).map(Some)
            }
            IteratorHelperStage::NextCall
            | IteratorHelperStage::DoneGet
            | IteratorHelperStage::ValueGet => {
                self.complete_iterator_helper(parent.first())?;
                self.throw_value(thrown, site.call_site).map(Some)
            }
            IteratorHelperStage::CreateMapNextGet
            | IteratorHelperStage::CreateFilterNextGet
            | IteratorHelperStage::CreateTakeNextGet
            | IteratorHelperStage::CreateDropNextGet
            | IteratorHelperStage::NormalCloseReturnGet
            | IteratorHelperStage::NormalCloseReturnCall => {
                self.throw_value(thrown, site.call_site).map(Some)
            }
            IteratorHelperStage::CreateTakeLimitConversion
            | IteratorHelperStage::CreateDropLimitConversion => {
                self.begin_iterator_helper_throw_close(
                    site,
                    IteratorHelperStage::CreateCloseReturnGet,
                    parent.first(),
                    thrown,
                )?;
                Ok(Some(None))
            }
        }
    }

    /// Reports whether a failing conversion is owned by take/drop creation.
    pub(crate) fn iterator_helper_limit_conversion_pending(&self) -> bool {
        self.fiber.completions.last_native().is_some_and(|parent| {
            matches!(
                parent.kind(),
                NativeContinuationKind::IteratorHelper(
                    IteratorHelperStage::CreateTakeLimitConversion
                        | IteratorHelperStage::CreateDropLimitConversion
                )
            )
        })
    }

    /// Converts a synchronous conversion failure into throw-completion IteratorClose.
    pub(crate) fn handle_iterator_helper_limit_conversion_error(
        &mut self,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let parent = self.pop_native_continuation()?;
        let site = parent.site();
        self.begin_iterator_helper_creation_error(site, parent.first(), error)
    }

    /// Normalizes ToNumber, rejects invalid limits, then starts the cached next lookup.
    fn finish_iterator_limit_conversion(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorHelperStage,
        iterator: Value,
        number: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(number)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        let integer = if number == 0.0 { 0.0 } else { number.trunc() };
        if integer.is_nan() || integer < 0.0 {
            self.write(site.caller_base, site.destination, iterator)?;
            let original = self.create_native_error(NativeErrorKind::Range, None)?;
            let iterator = self.read(site.caller_base, site.destination)?;
            return self.begin_iterator_helper_throw_close(
                site,
                IteratorHelperStage::CreateCloseReturnGet,
                iterator,
                original,
            );
        }
        let encoded = if !integer.is_finite() || integer > MAX_SAFE_INTEGER as f64 {
            Value::from_f64(f64::INFINITY)
        } else {
            safe_integer_value(integer as u64)
        };
        let next_stage = if stage == IteratorHelperStage::CreateTakeLimitConversion {
            IteratorHelperStage::CreateTakeNextGet
        } else {
            IteratorHelperStage::CreateDropNextGet
        };
        let next = self.intern_intrinsic_name(b"next")?;
        self.dispatch_iterator_helper_get(
            site,
            next_stage,
            iterator,
            encoded,
            iterator,
            next.into(),
        )
    }

    /// Preserves a conversion error while closing the temporary direct iterator record.
    fn begin_iterator_helper_creation_error(
        &mut self,
        site: NativeContinuationSite,
        iterator: Value,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let original = match error {
            ExecutionError::HostThrown(value) => value,
            error => {
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                self.write(site.caller_base, site.destination, iterator)?;
                let original = self.create_native_error(kind, None)?;
                let iterator = self.read(site.caller_base, site.destination)?;
                return self.begin_iterator_helper_throw_close(
                    site,
                    IteratorHelperStage::CreateCloseReturnGet,
                    iterator,
                    original,
                );
            }
        };
        self.begin_iterator_helper_throw_close(
            site,
            IteratorHelperStage::CreateCloseReturnGet,
            iterator,
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
    fn begin_iterator_helper_throw_close(
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
    fn resume_iterator_helper_throw_close(
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
    fn resume_iterator_helper_normal_close_get(
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
    fn dispatch_iterator_helper_get(
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
    fn dispatch_iterator_helper_get_effect(
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
    fn call_iterator_helper(
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
    fn call_iterator_helper_effect(
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
    fn iterator_helper_effective_continuation(
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

/// Decodes the exact safe-integer limit or the internal positive-infinity sentinel.
fn iterator_helper_limit_from_value(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number == f64::INFINITY {
        Ok(u64::MAX)
    } else {
        Ok(number as u64)
    }
}
