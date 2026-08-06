//! Resumable `CreateListFromArrayLike` state shared by apply and Reflect forwarding.

use core::mem::size_of;

use tachyon_gc::{AllocationSpace, GcExternalMemory, GcRef, Trace, Tracer};
use tachyon_value::{Immediate, Value};

use crate::{
    CallSite, ExecutionError, Isolate, NativeContinuation, NativeContinuationSite, PropertyKey,
    PropertyRead, VmRoots,
};

/// The terminal consumer for one materialized argument list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArgumentListOperation {
    FunctionApply,
    ReflectApply,
    ReflectConstruct,
    Call,
    TailCall,
    DirectEval,
    Construct,
}

/// GC-owned state retained while an observable `length` or indexed `Get` is executing.
#[derive(Debug)]
pub(crate) struct PendingArgumentList {
    source: Value,
    target: Value,
    this_value: Value,
    new_target: Value,
    operation: ArgumentListOperation,
    length: u32,
    index: u32,
    reading_length: bool,
    arguments: Box<[Value]>,
}

impl Trace for PendingArgumentList {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.source.trace(tracer);
        self.target.trace(tracer);
        self.this_value.trace(tracer);
        self.new_target.trace(tracer);
        self.arguments.trace(tracer);
    }
}

impl GcExternalMemory for PendingArgumentList {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.arguments.len() * size_of::<Value>()
    }
}

#[derive(Clone, Copy)]
struct ArgumentListSnapshot {
    source: Value,
    target: Value,
    this_value: Value,
    new_target: Value,
    operation: ArgumentListOperation,
    length: u32,
    index: u32,
    reading_length: bool,
}

impl Isolate {
    /// Fast-paths compiler-private packed Arrays and falls back to the resumable property walker.
    pub(crate) fn begin_internal_spread_call(
        &mut self,
        site: &CallSite,
        source: Value,
        target: Value,
        this_value: Value,
        new_target: Value,
        operation: ArgumentListOperation,
    ) -> Result<(), ExecutionError> {
        let Some(arguments) = self.packed_array_values(source)? else {
            return self
                .begin_argument_list(site, source, target, this_value, new_target, operation);
        };
        let length_atom = self.length_atom()?;
        let length = self
            .get_data_property(source, length_atom)?
            .and_then(crate::numeric_value)
            .filter(|length| length.is_finite() && *length >= 0.0 && length.fract() == 0.0)
            .and_then(|length| usize::try_from(length as u64).ok());
        if length != Some(arguments.len()) {
            return self
                .begin_argument_list(site, source, target, this_value, new_target, operation);
        }
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.finish_internal_spread_call(
            continuation_site,
            target,
            this_value,
            new_target,
            arguments,
            operation,
        )
    }

    /// Freezes one packed argument snapshot in the existing immutable call-prefix representation.
    fn finish_internal_spread_call(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        this_value: Value,
        new_target: Value,
        arguments: Vec<Value>,
        operation: ArgumentListOperation,
    ) -> Result<(), ExecutionError> {
        if operation == ArgumentListOperation::DirectEval
            && self.finish_direct_eval_argument_list(&site, target, &arguments)?
        {
            return Ok(());
        }
        let argument_count = u32::try_from(arguments.len())
            .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?;
        let prefix = self.create_apply_argument_prefix(target, this_value, arguments)?;
        let call_site = CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: target,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: argument_count,
            argument_count,
            this_value,
            new_target,
            construct_receiver: None,
            call_site: site.call_site,
        };
        match operation {
            ArgumentListOperation::TailCall => self.tail_call(call_site),
            ArgumentListOperation::Call | ArgumentListOperation::DirectEval => self.call(call_site),
            ArgumentListOperation::Construct => self.construct_site(call_site),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Starts `CreateListFromArrayLike`, publishing state before the observable `Get(length)`.
    pub(crate) fn begin_argument_list(
        &mut self,
        site: &CallSite,
        source: Value,
        target: Value,
        this_value: Value,
        new_target: Value,
        operation: ArgumentListOperation,
    ) -> Result<(), ExecutionError> {
        let state = self.allocate_pending_argument_list(PendingArgumentList {
            source,
            target,
            this_value,
            new_target,
            operation,
            length: 0,
            index: 0,
            reading_length: true,
            arguments: Box::new([]),
        })?;
        let site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.advance_argument_list(site, state, None)
    }

    /// Resumes one suspended `length` or indexed getter without using the Rust call stack.
    pub(crate) fn resume_argument_list(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArgumentList>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.advance_argument_list(site, state, Some(value))
    }

    /// Advances synchronous reads until an accessor suspends or the target call/construct starts.
    fn advance_argument_list(
        &mut self,
        site: NativeContinuationSite,
        mut state: GcRef<PendingArgumentList>,
        mut returned: Option<Value>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        loop {
            let snapshot = self.argument_list_snapshot(state)?;
            if snapshot.reading_length {
                let length = match returned.take() {
                    Some(value) => self.argument_list_length(value)?,
                    None => {
                        let key = PropertyKey::Atom(self.length_atom()?);
                        match self.resolve_property_read(snapshot.source, key)? {
                            PropertyRead::Missing => 0,
                            PropertyRead::Data(value) => self.argument_list_length(value)?,
                            PropertyRead::Accessor(getter)
                                if getter.as_immediate() == Some(Immediate::Undefined) =>
                            {
                                0
                            }
                            PropertyRead::Accessor(callee) => {
                                return self.dispatch_argument_list_get(site, state, callee);
                            }
                        }
                    }
                };
                state = self.allocate_argument_list_values(snapshot, length)?;
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                )?;
                continue;
            }
            if let Some(value) = returned.take() {
                self.store_argument_list_value(state, snapshot.index, value)?;
            }
            let snapshot = self.argument_list_snapshot(state)?;
            if snapshot.index == snapshot.length {
                return self.finish_argument_list(site, state, snapshot);
            }
            let key =
                PropertyKey::Atom(self.safe_integer_property_atom(u64::from(snapshot.index))?);
            match self.resolve_property_read(snapshot.source, key)? {
                PropertyRead::Missing => self.store_argument_list_value(
                    state,
                    snapshot.index,
                    Value::from_immediate(Immediate::Undefined),
                )?,
                PropertyRead::Data(value) => {
                    self.store_argument_list_value(state, snapshot.index, value)?
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    self.store_argument_list_value(
                        state,
                        snapshot.index,
                        Value::from_immediate(Immediate::Undefined),
                    )?;
                }
                PropertyRead::Accessor(callee) => {
                    return self.dispatch_argument_list_get(site, state, callee);
                }
            }
        }
    }

    /// Calls one source getter after its pending list has been published as a GC root.
    fn dispatch_argument_list_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArgumentList>,
        callee: Value,
    ) -> Result<(), ExecutionError> {
        self.dispatch_property_callback(
            NativeContinuation::argument_list_get(site, Value::from_heap_ref(state.raw())),
            callee,
        )
        .map(|_| ())
    }

    /// Performs the current engine's bounded `ToLength` subset before allocating exact backing.
    fn argument_list_length(&mut self, value: Value) -> Result<u32, ExecutionError> {
        let number = self.convert_to_number(value)?;
        let number = number
            .as_i32()
            .map(f64::from)
            .or_else(|| number.as_f64())
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if number >= f64::from(u32::MAX) {
            return Err(ExecutionError::ArrayLengthOverflow);
        }
        Ok(number.floor() as u32)
    }

    /// Allocates immutable, exactly-sized backing once `length` has been observed.
    fn allocate_argument_list_values(
        &mut self,
        snapshot: ArgumentListSnapshot,
        length: u32,
    ) -> Result<GcRef<PendingArgumentList>, ExecutionError> {
        let length = usize::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(length)
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        values.resize(length, Value::from_immediate(Immediate::Undefined));
        self.allocate_pending_argument_list(PendingArgumentList {
            source: snapshot.source,
            target: snapshot.target,
            this_value: snapshot.this_value,
            new_target: snapshot.new_target,
            operation: snapshot.operation,
            length: u32::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)?,
            index: 0,
            reading_length: false,
            arguments: values.into_boxed_slice(),
        })
    }

    /// Completes the argument-list operation with one immutable forwarding prefix allocation.
    fn finish_argument_list(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArgumentList>,
        snapshot: ArgumentListSnapshot,
    ) -> Result<(), ExecutionError> {
        let arguments = self.copy_argument_list_values(state, snapshot.length)?;
        if snapshot.operation == ArgumentListOperation::DirectEval
            && self.finish_direct_eval_argument_list(&site, snapshot.target, &arguments)?
        {
            return Ok(());
        }
        let prefix =
            self.create_apply_argument_prefix(snapshot.target, snapshot.this_value, arguments)?;
        let call_site = CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: snapshot.target,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: snapshot.length,
            argument_count: snapshot.length,
            this_value: snapshot.this_value,
            new_target: snapshot.new_target,
            construct_receiver: None,
            call_site: site.call_site,
        };
        match snapshot.operation {
            ArgumentListOperation::FunctionApply
            | ArgumentListOperation::ReflectApply
            | ArgumentListOperation::Call
            | ArgumentListOperation::DirectEval => self.call(call_site).map(|_| ()),
            ArgumentListOperation::TailCall => self.tail_call(call_site).map(|_| ()),
            ArgumentListOperation::ReflectConstruct | ArgumentListOperation::Construct => {
                self.construct_site(call_site).map(|_| ())
            }
        }
    }

    /// Executes the direct-eval branch after every spread argument has already been evaluated.
    fn finish_direct_eval_argument_list(
        &mut self,
        site: &NativeContinuationSite,
        callee: Value,
        arguments: &[Value],
    ) -> Result<bool, ExecutionError> {
        let is_current_eval = matches!(
            self.resolve_function_executable(callee)?,
            crate::FunctionExecutable::Native(crate::NativeFunction::HostEvalScript)
        ) && self.realm_for_callable(callee)? == self.active_realm;
        if !is_current_eval {
            return Ok(false);
        }
        let source = arguments
            .first()
            .copied()
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_string_value(source) {
            self.write(site.caller_base, site.destination, source)?;
            return Ok(true);
        }
        let callback = self
            .eval_script_callback
            .ok_or(ExecutionError::UnsupportedDynamicFunctionConstructor)?;
        let strict_caller = self
            .fiber
            .frames
            .last()
            .is_some_and(|frame| frame.strictness == crate::FunctionStrictness::Strict);
        let result = callback(
            self,
            self.active_realm,
            crate::EvalKind::Direct { strict_caller },
            source,
        )?;
        self.write(site.caller_base, site.destination, result)?;
        Ok(true)
    }

    pub(crate) fn pending_argument_list_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingArgumentList>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_argument_list)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    pub(crate) fn pending_argument_list_source(
        &mut self,
        state: GcRef<PendingArgumentList>,
    ) -> Result<Value, ExecutionError> {
        Ok(self.argument_list_snapshot(state)?.source)
    }

    fn allocate_pending_argument_list(
        &mut self,
        pending: PendingArgumentList,
    ) -> Result<GcRef<PendingArgumentList>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_argument_list,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn argument_list_snapshot(
        &mut self,
        state: GcRef<PendingArgumentList>,
    ) -> Result<ArgumentListSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_argument_list)
                    .map(|pending| ArgumentListSnapshot {
                        source: pending.source,
                        target: pending.target,
                        this_value: pending.this_value,
                        new_target: pending.new_target,
                        operation: pending.operation,
                        length: pending.length,
                        index: pending.index,
                        reading_length: pending.reading_length,
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Writes a single collected argument and publishes its possible young edge to the barrier.
    fn store_argument_list_value(
        &mut self,
        state: GcRef<PendingArgumentList>,
        index: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_argument_list)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let slot = pending
                    .arguments
                    .get_mut(index as usize)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                *slot = value;
                pending.index = index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
                Ok(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Copies exact-capacity values after all observable reads have completed.
    fn copy_argument_list_values(
        &mut self,
        state: GcRef<PendingArgumentList>,
        length: u32,
    ) -> Result<Vec<Value>, ExecutionError> {
        let length = usize::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(length)
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_argument_list)
                    .map_err(ExecutionError::NoGcBorrow)?;
                values.extend_from_slice(&pending.arguments);
                Ok(())
            })
        })?;
        Ok(values)
    }
}
