//! Shared resumable driver for lazy Iterator Helper creation, stepping, and close semantics.

use super::super::*;
use super::{IteratorHelperKind, IteratorHelperState};

pub(super) enum IteratorHelperEffect {
    Settled,
    Resume(NativeContinuation, Value),
}

impl IteratorHelperEffect {
    #[inline(always)]
    pub(super) fn resumed(self) -> Option<(NativeContinuation, IteratorHelperStage, Value)> {
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
            IteratorHelperKind::Map | IteratorHelperKind::Filter | IteratorHelperKind::FlatMap
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
            IteratorHelperKind::FlatMap => IteratorHelperStage::CreateFlatMapNextGet,
            IteratorHelperKind::Take | IteratorHelperKind::Drop => {
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
        let (stage, next, iterator) = if snapshot.kind == IteratorHelperKind::FlatMap
            && !is_nullish(snapshot.inner_iterator)
        {
            (
                IteratorHelperStage::InnerNextCall,
                snapshot.inner_next,
                snapshot.inner_iterator,
            )
        } else {
            (
                IteratorHelperStage::NextCall,
                snapshot.outer_next,
                snapshot.outer_iterator,
            )
        };
        self.call_iterator_helper(
            Self::native_site(site),
            stage,
            helper,
            next,
            iterator,
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
        if snapshot.kind == IteratorHelperKind::FlatMap && !is_nullish(snapshot.inner_iterator) {
            return self.dispatch_iterator_helper_get(
                Self::native_site(site),
                IteratorHelperStage::InnerCloseReturnGet,
                helper,
                Value::from_immediate(Immediate::Undefined),
                snapshot.inner_iterator,
                return_key.into(),
            );
        }
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
                | IteratorHelperStage::CreateFilterNextGet
                | IteratorHelperStage::CreateFlatMapNextGet => {
                    let kind = if stage == IteratorHelperStage::CreateMapNextGet {
                        IteratorHelperKind::Map
                    } else if stage == IteratorHelperStage::CreateFilterNextGet {
                        IteratorHelperKind::Filter
                    } else {
                        IteratorHelperKind::FlatMap
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
                    if snapshot.kind == IteratorHelperKind::FlatMap {
                        if !self.is_object_value(value) {
                            return self.begin_iterator_helper_outer_error(
                                site,
                                helper,
                                ExecutionError::NotObject(value),
                            );
                        }
                        let symbol = self
                            .agent
                            .well_known_symbols
                            .iterator
                            .expect("Symbol.iterator initializes before flatMap");
                        let key = self.property_key(symbol)?;
                        self.dispatch_iterator_helper_get_effect(
                            site,
                            IteratorHelperStage::FlatMapIteratorMethodGet,
                            helper,
                            value,
                            value,
                            key,
                        )?
                    } else {
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
                }
                IteratorHelperStage::FlatMapIteratorMethodGet => {
                    let helper = continuation.first();
                    let mapped = continuation.second();
                    if is_nullish(value) {
                        let next = self.next_atom()?;
                        self.dispatch_iterator_helper_get_effect(
                            site,
                            IteratorHelperStage::FlatMapNextGet,
                            helper,
                            mapped,
                            mapped,
                            next.into(),
                        )?
                    } else {
                        if let Err(error) = self.resolve_function_object(value) {
                            return self.begin_iterator_helper_outer_error(site, helper, error);
                        }
                        self.call_iterator_helper_effect(
                            site,
                            IteratorHelperStage::FlatMapIteratorMethodCall,
                            helper,
                            value,
                            mapped,
                            Value::from_immediate(Immediate::Undefined),
                            &[],
                        )?
                    }
                }
                IteratorHelperStage::FlatMapIteratorMethodCall => {
                    let helper = continuation.first();
                    if !self.is_object_value(value) {
                        return self.begin_iterator_helper_outer_error(
                            site,
                            helper,
                            ExecutionError::NotObject(value),
                        );
                    }
                    let next = self.next_atom()?;
                    self.dispatch_iterator_helper_get_effect(
                        site,
                        IteratorHelperStage::FlatMapNextGet,
                        helper,
                        value,
                        value,
                        next.into(),
                    )?
                }
                IteratorHelperStage::FlatMapNextGet => {
                    let helper = continuation.first();
                    if let Err(error) = self.resolve_function_object(value) {
                        return self.begin_iterator_helper_outer_error(site, helper, error);
                    }
                    let reference = self.iterator_helper_reference(helper)?;
                    self.set_iterator_helper_inner(reference, continuation.second(), value)?;
                    self.call_iterator_helper_effect(
                        site,
                        IteratorHelperStage::InnerNextCall,
                        helper,
                        value,
                        continuation.second(),
                        Value::from_immediate(Immediate::Undefined),
                        &[],
                    )?
                }
                IteratorHelperStage::InnerNextCall => {
                    let helper = continuation.first();
                    if !self.is_object_value(value) {
                        return self.begin_iterator_helper_outer_error(
                            site,
                            helper,
                            ExecutionError::NotObject(value),
                        );
                    }
                    let done = self.done_atom()?;
                    self.dispatch_iterator_helper_get_effect(
                        site,
                        IteratorHelperStage::InnerDoneGet,
                        helper,
                        value,
                        value,
                        done.into(),
                    )?
                }
                IteratorHelperStage::InnerDoneGet => {
                    let helper = continuation.first();
                    if self.is_truthy_value(value)? {
                        let reference = self.iterator_helper_reference(helper)?;
                        let snapshot = self.iterator_helper_snapshot(reference)?;
                        self.set_iterator_helper_inner(
                            reference,
                            Value::from_immediate(Immediate::Undefined),
                            Value::from_immediate(Immediate::Undefined),
                        )?;
                        let counter = snapshot
                            .counter_or_limit
                            .checked_add(1)
                            .ok_or(ExecutionError::ArrayLengthOverflow)?;
                        self.set_iterator_helper_counter(reference, counter)?;
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
                        let key = self.value_atom()?;
                        self.dispatch_iterator_helper_get_effect(
                            site,
                            IteratorHelperStage::InnerValueGet,
                            helper,
                            result,
                            result,
                            key.into(),
                        )?
                    }
                }
                IteratorHelperStage::InnerValueGet => {
                    let helper = continuation.first();
                    self.set_iterator_helper_state(
                        self.iterator_helper_reference(helper)?,
                        IteratorHelperState::SuspendedYield,
                    )?;
                    let result = self.create_iterator_result(value, false)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                IteratorHelperStage::InnerCloseReturnGet => {
                    let helper = continuation.first();
                    if is_nullish(value) {
                        let snapshot = self.iterator_helper_value_snapshot(helper)?;
                        let key = self.intern_intrinsic_name(b"return")?;
                        return self.dispatch_iterator_helper_get(
                            site,
                            IteratorHelperStage::NormalCloseReturnGet,
                            helper,
                            Value::from_immediate(Immediate::Undefined),
                            snapshot.outer_iterator,
                            key.into(),
                        );
                    }
                    if let Err(error) = self.resolve_function_object(value) {
                        return self.begin_iterator_helper_outer_error(site, helper, error);
                    }
                    let inner = self.iterator_helper_value_snapshot(helper)?.inner_iterator;
                    self.call_iterator_helper_effect(
                        site,
                        IteratorHelperStage::InnerCloseReturnCall,
                        helper,
                        value,
                        inner,
                        Value::from_immediate(Immediate::Undefined),
                        &[],
                    )?
                }
                IteratorHelperStage::InnerCloseReturnCall => {
                    let helper = continuation.first();
                    if !self.is_object_value(value) {
                        return self.begin_iterator_helper_outer_error(
                            site,
                            helper,
                            ExecutionError::NotObject(value),
                        );
                    }
                    let snapshot = self.iterator_helper_value_snapshot(helper)?;
                    let key = self.intern_intrinsic_name(b"return")?;
                    return self.dispatch_iterator_helper_get(
                        site,
                        IteratorHelperStage::NormalCloseReturnGet,
                        helper,
                        Value::from_immediate(Immediate::Undefined),
                        snapshot.outer_iterator,
                        key.into(),
                    );
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
            | IteratorHelperStage::CreateFlatMapNextGet
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
            IteratorHelperStage::FlatMapIteratorMethodGet
            | IteratorHelperStage::FlatMapIteratorMethodCall
            | IteratorHelperStage::FlatMapNextGet
            | IteratorHelperStage::InnerNextCall
            | IteratorHelperStage::InnerDoneGet
            | IteratorHelperStage::InnerValueGet
            | IteratorHelperStage::InnerCloseReturnGet
            | IteratorHelperStage::InnerCloseReturnCall => {
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
