//! Resumable protocol and close driver shared by all eager Iterator Helpers.

use super::super::*;
use super::{IteratorEagerKind, IteratorEagerOperation};

enum IteratorEagerEffect {
    Settled,
    Resume(NativeContinuation, Value),
}

impl IteratorEagerEffect {
    #[inline(always)]
    fn resumed(self) -> Option<(NativeContinuation, IteratorEagerStage, Value)> {
        let Self::Resume(continuation, value) = self else {
            return None;
        };
        let NativeContinuationKind::IteratorEager(stage) = continuation.kind() else {
            unreachable!("eager Iterator effects retain their typed continuation")
        };
        Some((continuation, stage, value))
    }
}

impl Isolate {
    /// Validates arguments before reading `next`, then creates the shared eager operation.
    pub(crate) fn begin_iterator_eager(
        &mut self,
        site: &CallSite,
        kind: IteratorEagerKind,
    ) -> Result<(), ExecutionError> {
        let iterator = site.this_value;
        if !self.is_object_value(iterator) {
            return Err(ExecutionError::NotObject(iterator));
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let callback = if kind == IteratorEagerKind::ToArray {
            undefined
        } else {
            self.call_argument(site, 0)?.unwrap_or(undefined)
        };
        let initial = if kind == IteratorEagerKind::Reduce {
            self.call_argument(site, 1)?
        } else {
            None
        };
        let state = self.allocate_iterator_eager_operation(IteratorEagerOperation {
            iterator,
            next_method: undefined,
            callback,
            accumulator_or_output: initial.unwrap_or(undefined),
            current_value: undefined,
            counter: 0,
            kind,
            has_accumulator: initial.is_some(),
        })?;
        let state_value = Value::from_heap_ref(state.raw());
        let native_site = Self::native_site(site);
        if kind != IteratorEagerKind::ToArray && !self.is_callable_value(callback)? {
            self.write(site.caller_base, site.destination, state_value)?;
            let original = self.create_native_error(NativeErrorKind::Type, None)?;
            let state_value = self.read(site.caller_base, site.destination)?;
            return self.begin_iterator_eager_close(
                native_site,
                IteratorEagerStage::ThrowCloseReturnGet,
                state_value,
                original,
            );
        }
        let key = self.next_atom()?;
        self.dispatch_iterator_eager_get(
            native_site,
            IteratorEagerStage::NextGet,
            state_value,
            undefined,
            iterator,
            key.into(),
        )
    }

    /// Resumes one eager boundary and trampolines all synchronous steps in constant Rust stack.
    pub(crate) fn resume_iterator_eager(
        &mut self,
        mut continuation: NativeContinuation,
        mut stage: IteratorEagerStage,
        mut value: Value,
    ) -> Result<(), ExecutionError> {
        loop {
            let site = continuation.site();
            let state_value = continuation.first();
            let state = self.iterator_eager_reference(state_value)?;
            let effect = match stage {
                IteratorEagerStage::NextGet => {
                    self.resolve_function_object(value)?;
                    self.update_iterator_eager(state, |operation| operation.next_method = value)?;
                    let snapshot = self.iterator_eager_snapshot(state)?;
                    if snapshot.kind == IteratorEagerKind::ToArray {
                        self.write(site.caller_base, site.destination, state_value)?;
                        let prototype = self
                            .realm
                            .array_prototype
                            .expect("Array prototype initializes before Iterator helpers");
                        let output = self.create_array_object_with_prototype(prototype)?;
                        let state = self.iterator_eager_reference(
                            self.read(site.caller_base, site.destination)?,
                        )?;
                        self.update_iterator_eager(state, |operation| {
                            operation.accumulator_or_output = output;
                        })?;
                        let snapshot = self.iterator_eager_snapshot(state)?;
                        return self.call_iterator_eager(
                            site,
                            IteratorEagerStage::NextCall,
                            Value::from_heap_ref(state.raw()),
                            snapshot.next_method,
                            snapshot.iterator,
                            Value::from_immediate(Immediate::Undefined),
                            &[],
                        );
                    }
                    let snapshot = self.iterator_eager_snapshot(state)?;
                    self.call_iterator_eager_effect(
                        site,
                        IteratorEagerStage::NextCall,
                        state_value,
                        snapshot.next_method,
                        snapshot.iterator,
                        Value::from_immediate(Immediate::Undefined),
                        &[],
                    )?
                }
                IteratorEagerStage::NextCall => {
                    if !self.is_object_value(value) {
                        return Err(ExecutionError::NotObject(value));
                    }
                    let key = self.done_atom()?;
                    self.dispatch_iterator_eager_get_effect(
                        site,
                        IteratorEagerStage::DoneGet,
                        state_value,
                        value,
                        value,
                        key.into(),
                    )?
                }
                IteratorEagerStage::DoneGet => {
                    if self.is_truthy_value(value)? {
                        return self.finish_iterator_eager(site, state);
                    }
                    let result = continuation.second();
                    let key = self.value_atom()?;
                    self.dispatch_iterator_eager_get_effect(
                        site,
                        IteratorEagerStage::ValueGet,
                        state_value,
                        result,
                        result,
                        key.into(),
                    )?
                }
                IteratorEagerStage::ValueGet => {
                    let snapshot = self.iterator_eager_snapshot(state)?;
                    match snapshot.kind {
                        IteratorEagerKind::ToArray => {
                            let index = u32::try_from(snapshot.counter)
                                .map_err(|_| ExecutionError::ArrayLengthOverflow)?;
                            self.write(site.caller_base, site.destination, state_value)?;
                            if !self.set_dense_array_value(
                                snapshot.accumulator_or_output,
                                index,
                                value,
                            )? {
                                return Err(ExecutionError::ArrayLengthOverflow);
                            }
                            let state_value = self.read(site.caller_base, site.destination)?;
                            let state = self.iterator_eager_reference(state_value)?;
                            let snapshot = self.iterator_eager_snapshot(state)?;
                            let counter = snapshot
                                .counter
                                .checked_add(1)
                                .ok_or(ExecutionError::ArrayLengthOverflow)?;
                            self.update_iterator_eager(state, |operation| {
                                operation.counter = counter;
                            })?;
                            self.call_iterator_eager_effect(
                                site,
                                IteratorEagerStage::NextCall,
                                state_value,
                                snapshot.next_method,
                                snapshot.iterator,
                                Value::from_immediate(Immediate::Undefined),
                                &[],
                            )?
                        }
                        IteratorEagerKind::Reduce if !snapshot.has_accumulator => {
                            self.update_iterator_eager(state, |operation| {
                                operation.accumulator_or_output = value;
                                operation.has_accumulator = true;
                                operation.counter = 1;
                            })?;
                            self.call_iterator_eager_effect(
                                site,
                                IteratorEagerStage::NextCall,
                                state_value,
                                snapshot.next_method,
                                snapshot.iterator,
                                Value::from_immediate(Immediate::Undefined),
                                &[],
                            )?
                        }
                        kind => {
                            self.update_iterator_eager(state, |operation| {
                                operation.current_value = value;
                            })?;
                            let counter = safe_integer_value(snapshot.counter);
                            if kind == IteratorEagerKind::Reduce {
                                self.call_iterator_eager_effect(
                                    site,
                                    IteratorEagerStage::CallbackCall,
                                    state_value,
                                    snapshot.callback,
                                    Value::from_immediate(Immediate::Undefined),
                                    Value::from_immediate(Immediate::Undefined),
                                    &[snapshot.accumulator_or_output, value, counter],
                                )?
                            } else {
                                self.call_iterator_eager_effect(
                                    site,
                                    IteratorEagerStage::CallbackCall,
                                    state_value,
                                    snapshot.callback,
                                    Value::from_immediate(Immediate::Undefined),
                                    Value::from_immediate(Immediate::Undefined),
                                    &[value, counter],
                                )?
                            }
                        }
                    }
                }
                IteratorEagerStage::CallbackCall => {
                    let snapshot = self.iterator_eager_snapshot(state)?;
                    let short_result = match snapshot.kind {
                        IteratorEagerKind::Some if self.is_truthy_value(value)? => {
                            Some(boolean_value(true))
                        }
                        IteratorEagerKind::Every if !self.is_truthy_value(value)? => {
                            Some(boolean_value(false))
                        }
                        IteratorEagerKind::Find if self.is_truthy_value(value)? => {
                            Some(snapshot.current_value)
                        }
                        _ => None,
                    };
                    if let Some(result) = short_result {
                        return self.begin_iterator_eager_close(
                            site,
                            IteratorEagerStage::NormalCloseReturnGet,
                            state_value,
                            result,
                        );
                    }
                    let counter = snapshot
                        .counter
                        .checked_add(1)
                        .ok_or(ExecutionError::ArrayLengthOverflow)?;
                    self.update_iterator_eager(state, |operation| {
                        operation.counter = counter;
                        if operation.kind == IteratorEagerKind::Reduce {
                            operation.accumulator_or_output = value;
                        }
                    })?;
                    self.call_iterator_eager_effect(
                        site,
                        IteratorEagerStage::NextCall,
                        state_value,
                        snapshot.next_method,
                        snapshot.iterator,
                        Value::from_immediate(Immediate::Undefined),
                        &[],
                    )?
                }
                IteratorEagerStage::ThrowCloseReturnGet => {
                    return self.resume_iterator_eager_throw_close_get(continuation, value);
                }
                IteratorEagerStage::ThrowCloseReturnCall => {
                    return Err(ExecutionError::HostThrown(continuation.second()));
                }
                IteratorEagerStage::NormalCloseReturnGet => {
                    if is_nullish(value) {
                        return self.write(
                            site.caller_base,
                            site.destination,
                            continuation.second(),
                        );
                    }
                    self.resolve_function_object(value)?;
                    let snapshot = self.iterator_eager_snapshot(state)?;
                    self.call_iterator_eager_effect(
                        site,
                        IteratorEagerStage::NormalCloseReturnCall,
                        state_value,
                        value,
                        snapshot.iterator,
                        continuation.second(),
                        &[],
                    )?
                }
                IteratorEagerStage::NormalCloseReturnCall => {
                    if !self.is_object_value(value) {
                        return Err(ExecutionError::NotObject(value));
                    }
                    return self.write(site.caller_base, site.destination, continuation.second());
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

    /// Finishes natural exhaustion without invoking the iterator's `return` method.
    fn finish_iterator_eager(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<IteratorEagerOperation>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.iterator_eager_snapshot(state)?;
        let result = match snapshot.kind {
            IteratorEagerKind::Reduce if !snapshot.has_accumulator => {
                return Err(ExecutionError::NotObject(Value::from_immediate(
                    Immediate::Undefined,
                )));
            }
            IteratorEagerKind::Reduce => snapshot.accumulator_or_output,
            IteratorEagerKind::ToArray => {
                self.set_array_length_value(
                    snapshot.accumulator_or_output,
                    safe_integer_value(snapshot.counter),
                )?;
                snapshot.accumulator_or_output
            }
            IteratorEagerKind::ForEach => Value::from_immediate(Immediate::Undefined),
            IteratorEagerKind::Some => boolean_value(false),
            IteratorEagerKind::Every => boolean_value(true),
            IteratorEagerKind::Find => Value::from_immediate(Immediate::Undefined),
        };
        self.write(site.caller_base, site.destination, result)
    }

    /// Starts normal or throw-completion IteratorClose while retaining its original completion.
    fn begin_iterator_eager_close(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorEagerStage,
        state: Value,
        completion: Value,
    ) -> Result<(), ExecutionError> {
        self.write(site.caller_base, site.destination, state)?;
        let key = self.intern_intrinsic_name(b"return")?;
        let state = self.read(site.caller_base, site.destination)?;
        let iterator = self
            .iterator_eager_snapshot(self.iterator_eager_reference(state)?)?
            .iterator;
        self.dispatch_iterator_eager_get(site, stage, state, completion, iterator, key.into())
    }

    /// Preserves the original error across every throw-completion close failure.
    fn resume_iterator_eager_throw_close_get(
        &mut self,
        continuation: NativeContinuation,
        method: Value,
    ) -> Result<(), ExecutionError> {
        if is_nullish(method) || !self.is_callable_value(method)? {
            return Err(ExecutionError::HostThrown(continuation.second()));
        }
        let snapshot =
            self.iterator_eager_snapshot(self.iterator_eager_reference(continuation.first())?)?;
        self.call_iterator_eager(
            continuation.site(),
            IteratorEagerStage::ThrowCloseReturnCall,
            continuation.first(),
            method,
            snapshot.iterator,
            continuation.second(),
            &[],
        )
    }

    /// Performs one proxy/accessor-aware Get under an eager typed parent.
    fn dispatch_iterator_eager_get(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorEagerStage,
        state: Value,
        retained: Value,
        target: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let effect =
            self.dispatch_iterator_eager_get_effect(site, stage, state, retained, target, key)?;
        let Some((continuation, stage, value)) = effect.resumed() else {
            return Ok(());
        };
        self.resume_iterator_eager(continuation, stage, value)
    }

    /// Returns synchronous Get completion to the eager trampoline without recursion.
    fn dispatch_iterator_eager_get_effect(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorEagerStage,
        state: Value,
        retained: Value,
        target: Value,
        key: PropertyKey,
    ) -> Result<IteratorEagerEffect, ExecutionError> {
        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_eager(
                site, stage, state, retained,
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, target, target, key) {
            let continuation = self.pop_native_continuation()?;
            if stage == IteratorEagerStage::ThrowCloseReturnGet {
                return Err(ExecutionError::HostThrown(continuation.second()));
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth || self.fiber.completions.len() <= depth {
            return Ok(IteratorEagerEffect::Settled);
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        Ok(IteratorEagerEffect::Resume(continuation, returned))
    }

    /// Calls one iterator method/callback with an exact, immutable argument prefix.
    #[allow(
        clippy::too_many_arguments,
        reason = "the typed call boundary keeps stage, roots, receiver, and retained result explicit"
    )]
    fn call_iterator_eager(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorEagerStage,
        state: Value,
        callee: Value,
        receiver: Value,
        retained: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        let effect = self.call_iterator_eager_effect(
            site, stage, state, callee, receiver, retained, arguments,
        )?;
        let Some((continuation, stage, value)) = effect.resumed() else {
            return Ok(());
        };
        self.resume_iterator_eager(continuation, stage, value)
    }

    /// Bounces synchronous calls while yielding control for bytecode, Proxy, or generator calls.
    #[allow(
        clippy::too_many_arguments,
        reason = "the typed call boundary keeps stage, roots, receiver, and retained result explicit"
    )]
    fn call_iterator_eager_effect(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorEagerStage,
        state: Value,
        callee: Value,
        receiver: Value,
        retained: Value,
        arguments: &[Value],
    ) -> Result<IteratorEagerEffect, ExecutionError> {
        self.resolve_function_object(callee)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_eager(
                site, stage, state, retained,
            ))
            .map_err(Self::completion_stack_error)?;
        let prefix_result = if arguments.is_empty() {
            Ok(None)
        } else {
            let mut copied = Vec::new();
            if copied.try_reserve_exact(arguments.len()).is_err() {
                self.pop_native_continuation()?;
                return Err(ExecutionError::BoundArgumentAllocationFailed);
            }
            copied.extend_from_slice(arguments);
            self.create_apply_argument_prefix(callee, receiver, copied)
                .map(Some)
        };
        let prefix = match prefix_result {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
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
            if stage == IteratorEagerStage::CallbackCall {
                self.begin_iterator_eager_callback_error(continuation, error)?;
                return Ok(IteratorEagerEffect::Settled);
            }
            if stage == IteratorEagerStage::ThrowCloseReturnCall {
                return Err(ExecutionError::HostThrown(continuation.second()));
            }
            return Err(error);
        }
        let parent_is_active = self.fiber.completions.last_native().is_some_and(|parent| {
            parent.kind() == NativeContinuationKind::IteratorEager(stage) && parent.first() == state
        });
        if !parent_is_active {
            return Ok(IteratorEagerEffect::Settled);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("eager Iterator call publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(IteratorEagerEffect::Settled);
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        Ok(IteratorEagerEffect::Resume(continuation, returned))
    }

    /// Converts an immediate native callback error into throw-completion IteratorClose.
    fn begin_iterator_eager_callback_error(
        &mut self,
        continuation: NativeContinuation,
        error: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let original = match error {
            ExecutionError::HostThrown(value) => value,
            error => {
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                let site = continuation.site();
                self.write(site.caller_base, site.destination, continuation.first())?;
                let original = self.create_native_error(kind, None)?;
                let state = self.read(site.caller_base, site.destination)?;
                return self.begin_iterator_eager_close(
                    site,
                    IteratorEagerStage::ThrowCloseReturnGet,
                    state,
                    original,
                );
            }
        };
        self.begin_iterator_eager_close(
            continuation.site(),
            IteratorEagerStage::ThrowCloseReturnGet,
            continuation.first(),
            original,
        )
    }

    /// Routes bytecode/accessor throws according to protocol, callback, and close precedence.
    pub(crate) fn handle_iterator_eager_thrown(
        &mut self,
        continuation: NativeContinuation,
        thrown: Value,
    ) -> Result<Option<Option<RunOutcome>>, ExecutionError> {
        let parent = if matches!(
            continuation.kind(),
            NativeContinuationKind::IteratorEager(_)
        ) {
            Some(continuation)
        } else {
            self.fiber
                .completions
                .last_native()
                .filter(|parent| matches!(parent.kind(), NativeContinuationKind::IteratorEager(_)))
        };
        let Some(parent) = parent else {
            return Ok(None);
        };
        let NativeContinuationKind::IteratorEager(stage) = parent.kind() else {
            return Ok(None);
        };
        let site = parent.site();
        match stage {
            IteratorEagerStage::CallbackCall => {
                self.begin_iterator_eager_close(
                    site,
                    IteratorEagerStage::ThrowCloseReturnGet,
                    parent.first(),
                    thrown,
                )?;
                Ok(Some(None))
            }
            IteratorEagerStage::ThrowCloseReturnGet | IteratorEagerStage::ThrowCloseReturnCall => {
                self.throw_value(parent.second(), site.call_site).map(Some)
            }
            _ => self.throw_value(thrown, site.call_site).map(Some),
        }
    }
}
