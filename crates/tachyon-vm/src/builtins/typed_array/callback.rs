//! Resumable fixed Number TypedArray predicate and find-family callbacks.

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
        let snapshot = self.typed_array_snapshot(receiver)?;
        self.typed_array_backing(snapshot.buffer)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_callable_value(callback)? {
            return Err(ExecutionError::NonCallable(callback));
        }
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let mode = typed_array_callback_mode(kind);
        let cursor = if typed_array_callback_reverse(mode) {
            snapshot.length
        } else {
            0
        };
        let state = self.allocate_typed_array_callback_state(NativeCallState {
            values: [
                receiver,
                callback,
                this_argument,
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
                return self.write(
                    site.caller_base,
                    site.destination,
                    typed_array_callback_miss(pending.count),
                );
            }
            let index = if reverse { cursor - 1 } else { cursor };
            self.set_typed_array_callback_cursor(state, if reverse { index } else { index + 1 })?;
            let element =
                self.typed_array_callback_element(pending.values[CALLBACK_RECEIVER], index)?;
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

    /// Calls the predicate with `(element, index, receiver)` while continuation roots are live.
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
        let prefix = match self.create_apply_argument_prefix(
            pending.values[CALLBACK_FUNCTION],
            pending.values[CALLBACK_THIS_ARGUMENT],
            vec![
                element,
                safe_integer_value(index as u64),
                pending.values[CALLBACK_RECEIVER],
            ],
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
            argument_prefix_count: 3,
            argument_count: 3,
            this_value: pending.values[CALLBACK_THIS_ARGUMENT],
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
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
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
}

#[inline(always)]
const fn typed_array_callback_mode(kind: TypedArrayCallbackKind) -> u8 {
    match kind {
        TypedArrayCallbackKind::Every => CALLBACK_EVERY,
        TypedArrayCallbackKind::Some => CALLBACK_SOME,
        TypedArrayCallbackKind::Find => CALLBACK_FIND,
        TypedArrayCallbackKind::FindIndex => CALLBACK_FIND_INDEX,
        TypedArrayCallbackKind::FindLast => CALLBACK_FIND_LAST,
        TypedArrayCallbackKind::FindLastIndex => CALLBACK_FIND_LAST_INDEX,
    }
}

#[inline(always)]
const fn typed_array_callback_reverse(mode: u8) -> bool {
    matches!(mode, CALLBACK_FIND_LAST | CALLBACK_FIND_LAST_INDEX)
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
        _ => Value::from_immediate(Immediate::Undefined),
    }
}
