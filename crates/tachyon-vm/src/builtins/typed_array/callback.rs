//! Resumable fixed Number TypedArray callback iteration and reduction.

use super::*;
use crate::runtime::fiber::TypedArrayCallbackStage;

const CALLBACK_RECEIVER: usize = 0;
const CALLBACK_FUNCTION: usize = 1;
const CALLBACK_THIS_ARGUMENT: usize = 2;
const CALLBACK_LENGTH: usize = 3;
const CALLBACK_CURSOR: usize = 4;

const CALLBACK_EVERY: u8 = 40;
const CALLBACK_SOME: u8 = 41;
const CALLBACK_FIND: u8 = 42;
const CALLBACK_FIND_INDEX: u8 = 43;
const CALLBACK_FIND_LAST: u8 = 44;
const CALLBACK_FIND_LAST_INDEX: u8 = 45;
const CALLBACK_FOR_EACH: u8 = 46;
const CALLBACK_REDUCE_UNINITIALIZED: u8 = 47;
const CALLBACK_REDUCE_INITIALIZED: u8 = 48;
const CALLBACK_REDUCE_RIGHT_UNINITIALIZED: u8 = 49;
const CALLBACK_REDUCE_RIGHT_INITIALIZED: u8 = 50;

struct TypedArrayCallbackRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArrayCallbackRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the fixed view and callback before publishing the shared five-slot scan state.
    pub(crate) fn begin_typed_array_callback(
        &mut self,
        site: &CallSite,
        kind: TypedArrayCallbackKind,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let snapshot = self.validated_typed_array_snapshot(receiver)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_callable_value(callback)? {
            return Err(ExecutionError::NonCallable(callback));
        }
        let second_argument = self.call_argument(site, 1)?;
        let mode = typed_array_callback_mode(kind, second_argument.is_some());
        let retained = second_argument.unwrap_or(Value::from_immediate(Immediate::Undefined));
        let cursor = if typed_array_callback_reverse(mode) {
            snapshot.length
        } else {
            0
        };
        let state = self.allocate_typed_array_callback_state(NativeCallState {
            values: [
                receiver,
                callback,
                retained,
                Value::from_f64(snapshot.length as f64),
                Value::from_f64(cursor as f64),
            ],
            count: mode,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.advance_typed_array_callback(continuation_site, state)
    }

    /// Consumes one predicate completion and either short-circuits or resumes the live scan.
    pub(crate) fn resume_typed_array_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArrayCallbackStage,
        returned: Value,
        element: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            TypedArrayCallbackStage::Callback => {
                if self.finish_typed_array_callback(site, state, returned, element)? {
                    Ok(())
                } else {
                    self.advance_typed_array_callback(site, state)
                }
            }
        }
    }

    /// Iterates synchronous element reads in a loop and yields only while a callback frame runs.
    fn advance_typed_array_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        loop {
            let pending = self.native_call_state_snapshot(state)?;
            let length = typed_array_callback_integer(pending.values[CALLBACK_LENGTH])?;
            let cursor = typed_array_callback_integer(pending.values[CALLBACK_CURSOR])?;
            let reverse = typed_array_callback_reverse(pending.count);
            if (!reverse && cursor >= length) || (reverse && cursor == 0) {
                if typed_array_callback_is_uninitialized_reduce(pending.count) {
                    return Err(ExecutionError::ArrayReduceEmpty);
                }
                let result = if typed_array_callback_is_reduce(pending.count) {
                    pending.values[CALLBACK_THIS_ARGUMENT]
                } else {
                    typed_array_callback_miss(pending.count)
                };
                return self.write(site.caller_base, site.destination, result);
            }
            let index = if reverse { cursor - 1 } else { cursor };
            self.set_typed_array_callback_cursor(state, if reverse { index } else { index + 1 })?;
            let element =
                self.typed_array_callback_element(pending.values[CALLBACK_RECEIVER], index)?;
            if typed_array_callback_is_uninitialized_reduce(pending.count) {
                self.set_typed_array_callback_accumulator(state, element)?;
                continue;
            }
            let Some(returned) = self.call_typed_array_callback(site, state, element, index)?
            else {
                return Ok(());
            };
            if self.finish_typed_array_callback(site, state, returned, element)? {
                return Ok(());
            }
        }
    }

    /// Reads the current indexed value, mapping post-validation detach/OOB to `undefined`.
    fn typed_array_callback_element(
        &mut self,
        receiver: Value,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.typed_array_snapshot(receiver)?;
        if index >= snapshot.length {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        match self.typed_array_read_element(snapshot, index) {
            Ok(value) => Ok(value),
            Err(ExecutionError::DetachedArrayBuffer) => {
                Ok(Value::from_immediate(Immediate::Undefined))
            }
            Err(error) => Err(error),
        }
    }

    /// Calls an iteration callback with its mode-specific argument list and `this` value.
    fn call_typed_array_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        element: Value,
        index: usize,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::typed_array_callback(
                site,
                TypedArrayCallbackStage::Callback,
                Value::from_heap_ref(state.raw()),
                element,
            ))
            .map_err(Isolate::completion_stack_error)?;
        let reduce = typed_array_callback_is_reduce(pending.count);
        let this_argument = if reduce {
            Value::from_immediate(Immediate::Undefined)
        } else {
            pending.values[CALLBACK_THIS_ARGUMENT]
        };
        let arguments = if reduce {
            vec![
                pending.values[CALLBACK_THIS_ARGUMENT],
                element,
                safe_integer_value(index as u64),
                pending.values[CALLBACK_RECEIVER],
            ]
        } else {
            vec![
                element,
                safe_integer_value(index as u64),
                pending.values[CALLBACK_RECEIVER],
            ]
        };
        let argument_count = if reduce { 4 } else { 3 };
        let prefix = match self.create_apply_argument_prefix(
            pending.values[CALLBACK_FUNCTION],
            this_argument,
            arguments,
        ) {
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
            callee: pending.values[CALLBACK_FUNCTION],
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: argument_count,
            argument_count,
            this_value: this_argument,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("TypedArray callback publishes one callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        self.pop_native_continuation()?;
        self.read(site.caller_base, site.destination).map(Some)
    }

    /// Applies the selected short-circuit result using the index committed before callback entry.
    fn finish_typed_array_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        returned: Value,
        element: Value,
    ) -> Result<bool, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        if typed_array_callback_is_reduce(pending.count) {
            self.set_typed_array_callback_value(state, CALLBACK_THIS_ARGUMENT, returned)?;
            return Ok(false);
        }
        if pending.count == CALLBACK_FOR_EACH {
            return Ok(false);
        }
        let truthy = self.is_truthy_value(returned)?;
        let selected = match pending.count {
            CALLBACK_EVERY => !truthy,
            CALLBACK_SOME
            | CALLBACK_FIND
            | CALLBACK_FIND_INDEX
            | CALLBACK_FIND_LAST
            | CALLBACK_FIND_LAST_INDEX => truthy,
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        if !selected {
            return Ok(false);
        }
        let result = match pending.count {
            CALLBACK_EVERY => Value::from_immediate(Immediate::False),
            CALLBACK_SOME => Value::from_immediate(Immediate::True),
            CALLBACK_FIND | CALLBACK_FIND_LAST => element,
            CALLBACK_FIND_INDEX | CALLBACK_FIND_LAST_INDEX => {
                let cursor = typed_array_callback_integer(pending.values[CALLBACK_CURSOR])?;
                let index = if typed_array_callback_reverse(pending.count) {
                    cursor
                } else {
                    cursor - 1
                };
                safe_integer_value(index as u64)
            }
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(true)
    }

    /// Allocates fixed callback state while tracing all pending inputs through forced collection.
    fn allocate_typed_array_callback_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArrayCallbackRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Commits the next numeric cursor without publishing a managed heap edge.
    fn set_typed_array_callback_cursor(
        &mut self,
        state: GcRef<NativeCallState>,
        cursor: usize,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[CALLBACK_CURSOR] = Value::from_f64(cursor as f64);
                Ok(())
            })
        })
    }

    /// Publishes the first reduction element and marks the accumulator initialized.
    fn set_typed_array_callback_accumulator(
        &mut self,
        state: GcRef<NativeCallState>,
        accumulator: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.values[CALLBACK_THIS_ARGUMENT] = accumulator;
                state.count = match state.count {
                    CALLBACK_REDUCE_UNINITIALIZED => CALLBACK_REDUCE_INITIALIZED,
                    CALLBACK_REDUCE_RIGHT_UNINITIALIZED => CALLBACK_REDUCE_RIGHT_INITIALIZED,
                    _ => return Err(ExecutionError::MissingNativeContinuation),
                };
                Ok(())
            })?;
            scope
                .write_value_barrier(state, accumulator)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Replaces one traced callback-state value through the normal old-to-young barrier.
    fn set_typed_array_callback_value(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = value;
                Ok(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }
}

#[inline(always)]
fn typed_array_callback_mode(kind: TypedArrayCallbackKind, has_second_argument: bool) -> u8 {
    match (kind, has_second_argument) {
        (TypedArrayCallbackKind::Every, _) => CALLBACK_EVERY,
        (TypedArrayCallbackKind::Some, _) => CALLBACK_SOME,
        (TypedArrayCallbackKind::Find, _) => CALLBACK_FIND,
        (TypedArrayCallbackKind::FindIndex, _) => CALLBACK_FIND_INDEX,
        (TypedArrayCallbackKind::FindLast, _) => CALLBACK_FIND_LAST,
        (TypedArrayCallbackKind::FindLastIndex, _) => CALLBACK_FIND_LAST_INDEX,
        (TypedArrayCallbackKind::ForEach, _) => CALLBACK_FOR_EACH,
        (TypedArrayCallbackKind::Reduce, false) => CALLBACK_REDUCE_UNINITIALIZED,
        (TypedArrayCallbackKind::Reduce, true) => CALLBACK_REDUCE_INITIALIZED,
        (TypedArrayCallbackKind::ReduceRight, false) => CALLBACK_REDUCE_RIGHT_UNINITIALIZED,
        (TypedArrayCallbackKind::ReduceRight, true) => CALLBACK_REDUCE_RIGHT_INITIALIZED,
        (TypedArrayCallbackKind::Map | TypedArrayCallbackKind::Filter, _) => {
            unreachable!("map/filter use the TypedArray transform state machine")
        }
    }
}

#[inline(always)]
const fn typed_array_callback_reverse(mode: u8) -> bool {
    matches!(
        mode,
        CALLBACK_FIND_LAST
            | CALLBACK_FIND_LAST_INDEX
            | CALLBACK_REDUCE_RIGHT_UNINITIALIZED
            | CALLBACK_REDUCE_RIGHT_INITIALIZED
    )
}

#[inline(always)]
const fn typed_array_callback_is_reduce(mode: u8) -> bool {
    matches!(
        mode,
        CALLBACK_REDUCE_UNINITIALIZED
            | CALLBACK_REDUCE_INITIALIZED
            | CALLBACK_REDUCE_RIGHT_UNINITIALIZED
            | CALLBACK_REDUCE_RIGHT_INITIALIZED
    )
}

#[inline(always)]
const fn typed_array_callback_is_uninitialized_reduce(mode: u8) -> bool {
    matches!(
        mode,
        CALLBACK_REDUCE_UNINITIALIZED | CALLBACK_REDUCE_RIGHT_UNINITIALIZED
    )
}

#[inline(always)]
fn typed_array_callback_integer(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}

#[inline(always)]
const fn typed_array_callback_miss(mode: u8) -> Value {
    match mode {
        CALLBACK_EVERY => Value::from_immediate(Immediate::True),
        CALLBACK_SOME => Value::from_immediate(Immediate::False),
        CALLBACK_FIND | CALLBACK_FIND_LAST => Value::from_immediate(Immediate::Undefined),
        CALLBACK_FIND_INDEX | CALLBACK_FIND_LAST_INDEX => Value::from_i32(-1),
        CALLBACK_FOR_EACH => Value::from_immediate(Immediate::Undefined),
        _ => Value::from_immediate(Immediate::Undefined),
    }
}
